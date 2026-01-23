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

use alloy_primitives::U256;
use bytes::Bytes;
use libp2p::PeerId;
use vertex_primitives::{ChunkAddress, OverlayAddress};

// ============================================================================
// Client Events
// ============================================================================

/// Events emitted by the client behaviour.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    // ========================================================================
    // Pricing Protocol
    // ========================================================================
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

    // ========================================================================
    // Retrieval Protocol
    // ========================================================================
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
        /// The chunk data.
        data: Bytes,
        /// The postage stamp.
        stamp: Bytes,
    },

    /// A chunk retrieval request failed.
    RetrievalFailed {
        /// The peer we requested from.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// Error description.
        error: String,
    },

    // ========================================================================
    // PushSync Protocol
    // ========================================================================
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
        /// The chunk data.
        data: Bytes,
        /// The postage stamp.
        stamp: Bytes,
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
        signature: Bytes,
        /// The receipt nonce.
        nonce: Bytes,
        /// The peer's storage radius.
        storage_radius: u8,
    },

    /// A chunk push failed.
    PushFailed {
        /// The peer we pushed to.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// Error description.
        error: String,
    },

    // ========================================================================
    // Settlement
    // ========================================================================
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

    /// Received a cheque from a peer (SWAP settlement).
    ChequeReceived {
        /// The peer that sent the cheque.
        peer: OverlayAddress,
        /// The cheque beneficiary.
        beneficiary: Bytes,
        /// The chequebook address.
        chequebook: Bytes,
        /// Cumulative payout amount.
        cumulative_payout: Bytes,
        /// Cheque signature.
        signature: Bytes,
    },

    // ========================================================================
    // Connection Lifecycle
    // ========================================================================
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

    // ========================================================================
    // Errors
    // ========================================================================
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

// ============================================================================
// Client Commands
// ============================================================================

/// Commands accepted by the client behaviour.
#[derive(Debug, Clone)]
pub enum ClientCommand {
    // ========================================================================
    // Handler Lifecycle
    // ========================================================================
    /// Activate the handler for a peer after handshake completes.
    ///
    /// This is sent by the node when TopologyEvent::PeerAuthenticated is received.
    /// The handler transitions from dormant to active state.
    ActivatePeer {
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The peer's Swarm overlay address.
        overlay: OverlayAddress,
        /// Whether the peer is a full node.
        is_full_node: bool,
    },

    // ========================================================================
    // Pricing Protocol
    // ========================================================================
    /// Announce our payment threshold to a peer.
    ///
    /// The threshold value depends on peer type (full vs light) and configuration.
    AnnouncePricing {
        /// The peer to announce to.
        peer: OverlayAddress,
        /// The payment threshold to announce.
        threshold: U256,
    },

    // ========================================================================
    // Retrieval Protocol
    // ========================================================================
    /// Request a chunk from a peer.
    RetrieveChunk {
        /// The peer to request from.
        peer: OverlayAddress,
        /// The chunk address to retrieve.
        address: ChunkAddress,
    },

    /// Serve a chunk to a peer (response to ChunkRequested).
    ServeChunk {
        /// The peer to serve.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// The chunk data.
        data: Bytes,
        /// The postage stamp.
        stamp: Bytes,
    },

    // ========================================================================
    // PushSync Protocol
    // ========================================================================
    /// Push a chunk to a peer.
    PushChunk {
        /// The peer to push to.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// The chunk data.
        data: Bytes,
        /// The postage stamp.
        stamp: Bytes,
    },

    /// Send a receipt to a peer (response to ChunkPushReceived).
    SendReceipt {
        /// The peer to send the receipt to.
        peer: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// The receipt signature.
        signature: Bytes,
        /// The receipt nonce.
        nonce: Bytes,
        /// Our storage radius.
        storage_radius: u8,
    },

    // ========================================================================
    // Settlement
    // ========================================================================
    /// Send a cheque to a peer (SWAP settlement).
    SendCheque {
        /// The peer to send the cheque to.
        peer: OverlayAddress,
        /// The cheque to send.
        cheque: Cheque,
    },

    // ========================================================================
    // Connection Management
    // ========================================================================
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

/// A SWAP cheque for settlement.
#[derive(Debug, Clone)]
pub struct Cheque {
    /// The cheque beneficiary.
    pub beneficiary: Bytes,
    /// The chequebook address.
    pub chequebook: Bytes,
    /// Cumulative payout amount.
    pub cumulative_payout: Bytes,
    /// Cheque signature.
    pub signature: Bytes,
}
