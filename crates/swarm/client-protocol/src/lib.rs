//! Command and event contract for the client behaviour.
//!
//! The behaviour accepts [`ClientCommand`]s and emits [`ClientEvent`]s; settlement
//! events ([`PseudosettleEvent`], [`SwapEvent`]) are extracted for their services.
//! Lives below both the node and the settlement crates so neither depends up on the
//! other.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::string::String;

use alloy_primitives::U256;
use libp2p::PeerId;
use nectar_primitives::{AnyChunk, ChunkAddress};
use tokio::sync::oneshot;
use vertex_swarm_api::Au;
use vertex_swarm_net_pushsync::Receipt;
#[cfg(feature = "swap")]
use vertex_swarm_net_swap::SignedCheque;
use vertex_swarm_primitives::{OverlayAddress, Stamp, StampedChunk, SwarmNodeType};

/// Channel on which an outbound retrieval request resolves.
///
/// The sender travels with the request to the outbound substream; dropping it
/// anywhere on that path surfaces as [`ChunkTransferError::Cancelled`].
pub type RetrievalResponseTx = oneshot::Sender<Result<RetrievalResult, ChunkTransferError>>;

/// Channel on which an outbound chunk push resolves.
///
/// A receipt whose storer cannot be recovered is rejected at decode and surfaces
/// here as an error, never as a value.
pub type PushResponseTx = oneshot::Sender<Result<Receipt, ChunkTransferError>>;

/// Result of a chunk retrieval.
///
/// The chunk is address-validated at decode, so it answers the request
/// regardless of the stamp. The stamp is optional: a storer may omit it from the
/// delivery, and it is never re-read on this path.
#[derive(Debug)]
pub struct RetrievalResult {
    pub chunk: AnyChunk,
    /// The postage stamp the responder attached, if any.
    pub stamp: Option<Stamp>,
    /// The peer that served the chunk.
    pub peer: OverlayAddress,
}

/// A pseudosettle acknowledgement in domain terms.
///
/// The `client-behaviour` wire boundary converts to and from the on-wire ack:
/// `accepted` is the amount the responder granted, and `timestamp` is the
/// Unix-nanosecond clock the responder sampled when deciding it. The receiver
/// refreshes its allowance against `timestamp`, so the sampling point stays in
/// the deciding service and is never re-sampled at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PseudosettleAck {
    /// The amount accepted by the responder.
    pub accepted: Au,
    /// Unix nanoseconds the responder sampled when deciding the accepted amount.
    pub timestamp: i64,
}

/// Outcome error shared by both chunk transfer operations.
///
/// Both retrieval and push resolve through this type; most variants surface from
/// either path.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ChunkTransferError {
    #[error("Network channel closed")]
    ChannelClosed,
    #[error("Peer not connected")]
    NotConnected,
    #[error("Request cancelled")]
    Cancelled,
    /// The peer did not complete the request within the per-protocol deadline
    /// (`retrieval_timeout` / `pushsync_timeout`). The liveness boundary against
    /// a withholding peer; retryable against another candidate.
    #[error("Request timed out")]
    TimedOut,
    /// Local protocol failure (dial, stream, or inactive handler). Remote-side
    /// failures are carried by [`Self::Remote`].
    #[error("Protocol error: {0}")]
    Protocol(String),
    /// The remote reported a failure. The reason is not carried: the remote's
    /// error string is adversarial input we never read.
    #[error("Remote peer reported a failure")]
    Remote,

    /// Retrieval only.
    #[error("Chunk not found: {0}")]
    NotFound(ChunkAddress),

    /// The local credit gate refused the request at the peer's disconnect line.
    /// No bytes were sent and a settle was triggered so the peer drains; another
    /// candidate should be tried.
    #[error("Admission refused at the disconnect line")]
    Refused,
}

impl ChunkTransferError {
    /// Whether retrying the request against another candidate may succeed.
    ///
    /// Timeout, remote failure, transient protocol error, not-found, and a local
    /// credit refusal are retryable (another candidate may hold the chunk or be
    /// affordable); a cancelled or channel-closed request reflects a local
    /// teardown that another attempt cannot fix.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::TimedOut
            | Self::Remote
            | Self::Protocol(_)
            | Self::NotFound(_)
            | Self::Refused => true,
            Self::ChannelClosed | Self::NotConnected | Self::Cancelled => false,
        }
    }

    /// Whether a dispatch-committed origin debit must be refunded.
    ///
    /// True only when the responder provably did not charge: a peer that
    /// answered with an explicit not-found / error delivery, or a request that
    /// never reached a connected peer. Every other post-dispatch outcome (reset,
    /// timeout, cancel, local protocol error) may have moved bytes and charged,
    /// so the debit stays committed to keep our debt-view at or above the server's.
    pub fn is_confirmed_absent(&self) -> bool {
        match self {
            Self::NotFound(_) | Self::NotConnected => true,
            Self::ChannelClosed
            | Self::Cancelled
            | Self::TimedOut
            | Self::Protocol(_)
            | Self::Remote
            | Self::Refused => false,
        }
    }
}

/// Why a retrieval or pushsync request failed, classified for peer scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The peer delivered or pushed a chunk that failed address or stamp
    /// reconstruction. Scored as invalid data.
    InvalidChunk,
    /// A transport, negotiation, timeout, or storer-reported failure that is
    /// not evidence of malformed data. Scored as a plain failure.
    Protocol,
}

/// Events emitted by the client behaviour.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Received a payment threshold from a peer.
    PricingReceived {
        /// The peer's overlay address.
        peer: OverlayAddress,
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The payment threshold announced by the peer.
        threshold: U256,
    },

    /// Successfully sent our payment threshold to a peer.
    PricingSent {
        /// The peer we sent the threshold to.
        peer: OverlayAddress,
    },

    /// We served an inbound retrieval request from our cache (scoring/metrics only).
    InboundServed {
        /// The peer we served.
        peer: OverlayAddress,
    },

    /// We answered an inbound retrieval by forwarding to a closer peer.
    InboundForwarded {
        /// The peer we served.
        peer: OverlayAddress,
    },

    /// We could not serve or forward an inbound retrieval; the substream reset.
    InboundMissed {
        /// The peer that asked.
        peer: OverlayAddress,
        /// The requested chunk address.
        address: ChunkAddress,
    },

    /// We relayed a storer's receipt for an inbound pushsync (never signed it).
    InboundRelayed {
        /// The peer that pushed.
        peer: OverlayAddress,
    },

    /// We stored an inbound pushsync delivery in the reserve and acknowledged it
    /// with our own signed receipt. Reached only on a storer responsible for the
    /// chunk (scoring/metrics only).
    InboundStored {
        /// The peer that pushed.
        peer: OverlayAddress,
    },

    /// We could not forward an inbound pushsync (or, on the storer ingest path,
    /// could not store or acknowledge it); the substream reset.
    InboundPushFailed {
        /// The peer that pushed.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
    },

    /// Received a chunk from a peer (response to our request).
    ChunkReceived {
        /// The peer that sent the chunk.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// The received chunk.
        chunk: AnyChunk,
        /// The postage stamp the responder attached, if any.
        stamp: Option<Stamp>,
        /// Time from request to delivery, for latency scoring.
        latency: core::time::Duration,
        /// True if this was our own request, false if a relay leg. Only an
        /// origin delivery is debited; the forwarder debits its own legs.
        originated: bool,
    },

    /// A chunk retrieval request failed.
    RetrievalFailed {
        /// The peer we requested from.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// Error description.
        error: String,
        /// Whether the failure was a malformed chunk (vs a plain failure).
        kind: FailureKind,
    },

    /// Received a receipt for a chunk we pushed.
    ReceiptReceived {
        /// The peer that sent the receipt.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// Time from push to receipt, for latency scoring.
        latency: core::time::Duration,
        /// True if this was our own push, false if a relay leg. Only an origin
        /// receipt is debited; the forwarder debits its own legs.
        originated: bool,
    },

    /// A chunk push failed.
    PushFailed {
        /// The peer we pushed to.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// Error description.
        error: String,
        /// Whether the failure was a malformed chunk (vs a plain failure).
        kind: FailureKind,
    },

    /// A peer sent malformed data on an inbound substream; rejected at decode and
    /// scored adversely for invalid data.
    InboundInvalidData {
        /// The peer that sent the malformed data.
        peer: OverlayAddress,
        /// The protocol that rejected the data.
        protocol: &'static str,
    },

    /// Received a pseudosettle payment from a peer.
    PseudosettleReceived {
        /// The peer that sent the payment.
        peer: OverlayAddress,
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The payment amount.
        amount: U256,
        /// Request ID for sending ack.
        request_id: u64,
    },

    /// Successfully sent a pseudosettle payment.
    PseudosettleSent {
        /// The peer we sent to.
        peer: OverlayAddress,
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The ack received.
        ack: PseudosettleAck,
    },

    /// Received a swap cheque from a peer.
    #[cfg(feature = "swap")]
    SwapChequeReceived {
        /// The peer that sent the cheque.
        peer: OverlayAddress,
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The signed cheque received.
        cheque: SignedCheque,
        /// The peer's advertised exchange rate, from the headers exchange.
        peer_rate: U256,
    },

    /// Successfully sent a swap cheque to a peer.
    #[cfg(feature = "swap")]
    SwapChequeSent {
        /// The peer we sent the cheque to.
        peer: OverlayAddress,
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The peer's advertised exchange rate, from the headers exchange.
        peer_rate: U256,
    },

    /// A peer's handler has been activated (after [`ClientCommand::ActivatePeer`]).
    PeerActivated {
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The peer's overlay address.
        overlay: OverlayAddress,
    },

    /// A peer has disconnected.
    PeerDisconnected {
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The peer's overlay address.
        overlay: OverlayAddress,
    },

    /// A protocol error occurred.
    ProtocolError {
        /// The peer involved (if known).
        peer: Option<OverlayAddress>,
        /// The libp2p peer ID (if known).
        peer_id: Option<PeerId>,
        /// The protocol that failed.
        protocol: &'static str,
        /// Error description.
        error: String,
    },
}

/// Commands accepted by the client behaviour.
///
/// Request commands ([`Self::RetrieveChunk`], [`Self::PushChunk`]) carry their
/// response channel, so the enum is intentionally not `Clone`.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum ClientCommand {
    /// Activate the handler for a peer after handshake completes, transitioning it
    /// from dormant to active.
    ActivatePeer {
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The peer's Swarm overlay address.
        overlay: OverlayAddress,
        /// The peer's node type.
        node_type: SwarmNodeType,
    },

    /// Announce our payment threshold to a peer.
    AnnouncePricing {
        /// The peer to announce to.
        peer: OverlayAddress,
        /// The payment threshold to announce.
        threshold: U256,
    },

    /// Request a chunk from a peer.
    RetrieveChunk {
        /// The peer to request from.
        peer: OverlayAddress,
        /// The chunk address to retrieve.
        address: ChunkAddress,
        /// Resolves with the retrieved chunk or the failure.
        response: RetrievalResponseTx,
        /// True for our own request, false for a forwarder relay leg. Echoed
        /// back on the completion event so only origin requests are debited.
        originated: bool,
    },

    /// Push a chunk to a peer.
    PushChunk {
        /// The peer to push to.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// The chunk and its postage stamp to push.
        chunk: StampedChunk,
        /// Resolves with the storer's receipt or the failure.
        response: PushResponseTx,
        /// True for our own push, false for a forwarder relay leg. Echoed back
        /// on the completion event so only origin pushes are debited.
        originated: bool,
    },

    /// Send a pseudosettle payment to a peer.
    SendPseudosettle {
        /// The peer to send the payment to.
        peer: OverlayAddress,
        /// The amount to pay.
        amount: U256,
    },

    /// Acknowledge a pseudosettle payment.
    AckPseudosettle {
        /// The peer to ack.
        peer: OverlayAddress,
        /// Request ID from the received payment.
        request_id: u64,
        /// The ack to send.
        ack: PseudosettleAck,
    },

    /// Send a swap cheque to a peer.
    #[cfg(feature = "swap")]
    SendCheque {
        /// The peer to send the cheque to.
        peer: OverlayAddress,
        /// The signed cheque to send.
        cheque: SignedCheque,
    },
}

/// Events extracted from [`ClientEvent`] and routed to the pseudosettle service.
#[derive(Debug, Clone)]
pub enum PseudosettleEvent {
    /// We sent a pseudosettle and received an ack.
    Sent {
        /// The peer we settled with.
        peer: OverlayAddress,
        /// The acknowledgment received.
        ack: PseudosettleAck,
    },
    /// A peer sent us a pseudosettle request.
    Received {
        /// The peer that sent the request.
        peer: OverlayAddress,
        /// The payment amount requested.
        amount: U256,
        /// Request ID for sending ack.
        request_id: u64,
    },
    /// An outbound pseudosettle substream failed or the peer disconnected before
    /// an ack arrived; any pending settle for this peer must be released.
    Failed {
        /// The peer whose pending settle can no longer complete.
        peer: OverlayAddress,
    },
}

/// Events extracted from [`ClientEvent`] and routed to the swap settlement service.
#[cfg(feature = "swap")]
#[derive(Debug, Clone)]
pub enum SwapEvent {
    /// We sent a cheque and the headers exchange completed.
    ChequeSent {
        /// The peer we sent the cheque to.
        peer: OverlayAddress,
        /// The peer's advertised exchange rate, from the headers exchange.
        peer_rate: U256,
    },
    /// A peer sent us a cheque.
    ChequeReceived {
        /// The peer that sent the cheque.
        peer: OverlayAddress,
        /// The signed cheque received.
        cheque: SignedCheque,
        /// The peer's advertised exchange rate, from the headers exchange.
        peer_rate: U256,
    },
    /// An outbound swap substream failed or the peer disconnected before the
    /// cheque-sent ack arrived; any pending settle for this peer must be released.
    Failed {
        /// The peer whose pending settle can no longer complete.
        peer: OverlayAddress,
    },
}
