//! Events and commands for the client behaviour.
//!
//! The client behaviour emits [`ClientEvent`]s and accepts [`ClientCommand`]s.
//!
//! # Design
//!
//! The client behaviour handles:
//! - Protocol negotiation and stream management
//! - Message encoding/decoding
//! - Per-peer connection state
//!
//! # Settlement Events
//!
//! Settlement-specific events ([`PseudosettleEvent`]) are defined here
//! for routing to the respective settlement services. The behaviour routes these
//! events based on optional senders configured at construction time.

use alloy_primitives::{Signature, U256};
use libp2p::PeerId;
use nectar_primitives::{ChunkAddress, Nonce};
use tokio::sync::oneshot;
use vertex_swarm_api::PushReceipt;
use vertex_swarm_net_pseudosettle::PaymentAck;
#[cfg(feature = "swap")]
use vertex_swarm_net_swap::SignedCheque;
use vertex_swarm_primitives::{OverlayAddress, StampedChunk, StorageRadius, SwarmNodeType};

use crate::client_service::{ChunkTransferError, RetrievalResult};

/// Channel on which an outbound retrieval request resolves.
///
/// The sender travels with the request from the caller through the behaviour
/// and handler into the outbound substream state, so the response (or any
/// failure along the way) resolves the caller directly. Dropping the sender
/// anywhere on that path surfaces as [`ChunkTransferError::Cancelled`].
pub type RetrievalResponseTx = oneshot::Sender<Result<RetrievalResult, ChunkTransferError>>;

/// Channel on which an outbound chunk push resolves.
///
/// Same lifecycle as [`RetrievalResponseTx`]: the storer's receipt or the
/// failure that prevented it resolves the caller directly.
pub type PushResponseTx = oneshot::Sender<Result<PushReceipt, ChunkTransferError>>;

/// Why a retrieval or pushsync request failed, classified for peer scoring.
///
/// Derived from the typed codec error at the point the failure is observed, so
/// the client service never parses error strings to decide how to score a peer.
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
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Received a payment threshold from a peer.
    ///
    /// Validate this threshold and decide whether to continue or disconnect.
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

    /// A peer is requesting a chunk from us.
    ///
    /// Check if we have the chunk, verify accounting, then respond with
    /// `ServeChunk` command.
    ChunkRequested {
        /// The peer requesting the chunk.
        peer: OverlayAddress,
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The requested chunk address.
        address: ChunkAddress,
        /// Request ID for matching response.
        request_id: u64,
    },

    /// Received a chunk from a peer (response to our request).
    ///
    /// Record the bandwidth usage for accounting.
    ChunkReceived {
        /// The peer that sent the chunk.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// The received chunk and its postage stamp.
        chunk: StampedChunk,
        /// Time from request to delivery, for latency scoring.
        latency: core::time::Duration,
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

    /// A peer is pushing a chunk to us.
    ///
    /// Validate the stamp, decide whether to store or forward, then respond
    /// with `SendReceipt` command.
    ChunkPushReceived {
        /// The peer pushing the chunk.
        peer: OverlayAddress,
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The chunk address.
        address: ChunkAddress,
        /// The pushed chunk and its postage stamp.
        chunk: StampedChunk,
        /// Request ID for matching response.
        request_id: u64,
    },

    /// Received a receipt for a chunk we pushed.
    ReceiptReceived {
        /// The peer that sent the receipt.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// The receipt signature.
        signature: Signature,
        /// The receipt nonce.
        nonce: Nonce,
        /// The peer's storage radius.
        storage_radius: StorageRadius,
        /// Time from push to receipt, for latency scoring.
        latency: core::time::Duration,
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

    /// A peer sent us malformed data on an inbound substream.
    ///
    /// The chunk or request failed reconstruction at decode and was rejected;
    /// the sender is scored adversely for invalid data.
    InboundInvalidData {
        /// The peer that sent the malformed data.
        peer: OverlayAddress,
        /// The protocol that rejected the data.
        protocol: &'static str,
    },

    /// A settlement is needed with a peer.
    ///
    /// Emitted when the balance crosses the payment threshold. Initiate
    /// swap or pseudosettle accordingly.
    SettlementNeeded {
        /// The peer to settle with.
        peer: OverlayAddress,
        /// Current balance (positive = they owe us).
        balance: i64,
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
        ack: PaymentAck,
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

    /// A peer's handler has been activated.
    ///
    /// This is emitted after the ActivatePeer command is processed.
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
/// Request commands ([`Self::RetrieveChunk`], [`Self::PushChunk`]) carry the
/// response channel for their outcome, so the enum is intentionally not
/// `Clone`.
#[derive(Debug)]
pub enum ClientCommand {
    /// Activate the handler for a peer after handshake completes.
    ///
    /// This is sent by the node when TopologyEvent::PeerAuthenticated is received.
    /// The handler transitions from dormant to active state.
    ActivatePeer {
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The peer's Swarm overlay address.
        overlay: OverlayAddress,
        /// The peer's node type.
        node_type: SwarmNodeType,
    },

    /// Announce our payment threshold to a peer.
    ///
    /// The threshold value depends on the peer's node type (Storer vs Client)
    /// and configuration.
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
    },

    /// Serve a chunk to a peer (response to ChunkRequested).
    ServeChunk {
        /// The peer to serve.
        peer: OverlayAddress,
        /// Request ID from ChunkRequested event.
        request_id: u64,
        /// The chunk address.
        address: ChunkAddress,
        /// The chunk and its postage stamp to serve.
        chunk: StampedChunk,
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
    },

    /// Send a receipt to a peer (response to ChunkPushReceived).
    SendReceipt {
        /// The peer to send the receipt to.
        peer: OverlayAddress,
        /// Request ID from ChunkPushReceived event.
        request_id: u64,
        /// The chunk address.
        address: ChunkAddress,
        /// The receipt signature.
        signature: Signature,
        /// The receipt nonce.
        nonce: Nonce,
        /// Our storage radius.
        storage_radius: StorageRadius,
    },

    /// Fail an inbound retrieval request (response to ChunkRequested).
    ///
    /// Resets the request substream without sending a delivery, which the
    /// requester reads as a failed retrieval. Used on a local-store miss (or
    /// when we have no local store) until forwarding to a closer peer lands.
    FailRetrieval {
        /// The peer whose request failed.
        peer: OverlayAddress,
        /// Request ID from the ChunkRequested event.
        request_id: u64,
    },

    /// Fail an inbound push (response to ChunkPushReceived).
    ///
    /// Resets the request substream without sending a receipt, which the
    /// pusher reads as a failed push. Used when we are not responsible for the
    /// address (or have no local store) until relaying the push lands.
    FailPush {
        /// The peer whose push failed.
        peer: OverlayAddress,
        /// Request ID from the ChunkPushReceived event.
        request_id: u64,
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
        ack: PaymentAck,
    },

    /// Send a swap cheque to a peer.
    #[cfg(feature = "swap")]
    SendCheque {
        /// The peer to send the cheque to.
        peer: OverlayAddress,
        /// The signed cheque to send.
        cheque: SignedCheque,
    },

    /// Disconnect from a peer.
    ///
    /// Used when a peer fails validation (e.g., threshold too low).
    DisconnectPeer {
        /// The peer to disconnect.
        peer: OverlayAddress,
        /// Reason for disconnection.
        reason: Option<String>,
    },
}

/// Events routed to the pseudosettle service.
///
/// These events are extracted from [`ClientEvent`] and sent to the
/// pseudosettle service via a dedicated channel for type-safe handling.
#[derive(Debug, Clone)]
pub enum PseudosettleEvent {
    /// We sent a pseudosettle and received an ack.
    Sent {
        /// The peer we settled with.
        peer: OverlayAddress,
        /// The acknowledgment received.
        ack: PaymentAck,
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
}

/// Events routed to the swap settlement service.
///
/// These events are extracted from [`ClientEvent`] and sent to the swap
/// service via a dedicated channel for type-safe handling. They carry strong
/// types ([`SignedCheque`], typed peer, typed rate) so the service never sees
/// raw wire bytes.
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
}
