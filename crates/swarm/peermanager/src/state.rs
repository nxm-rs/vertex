//! Peer state types.
//!
//! This module defines the core peer state management types:
//! - [`PeerState`]: Connection lifecycle state machine
//! - [`PeerInfo`]: Runtime peer information (in-memory)
//! - [`StoredPeer`]: Persistable peer data including BzzAddress

use alloy_primitives::B256;
use libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use vertex_swarm_peer::SwarmPeer;
use web_time::Instant;

use crate::score::PeerScoreSnapshot;

/// Current state of a peer in the peer manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerState {
    /// Peer is known but not connected.
    Known,
    /// Connection attempt in progress.
    Connecting,
    /// Connected and handshake complete.
    Connected,
    /// Was connected, now disconnected. May reconnect.
    Disconnected,
    /// Banned. Will not reconnect.
    Banned,
}

impl PeerState {
    /// Returns true if the peer is currently connected.
    pub fn is_connected(&self) -> bool {
        matches!(self, PeerState::Connected)
    }

    /// Returns true if the peer can be dialed.
    pub fn is_dialable(&self) -> bool {
        matches!(self, PeerState::Known | PeerState::Disconnected)
    }

    /// Returns true if the peer is banned.
    pub fn is_banned(&self) -> bool {
        matches!(self, PeerState::Banned)
    }
}

/// Runtime information about a peer (in-memory, not persisted).
///
/// This contains transient state that doesn't need persistence,
/// like connection state and timing information.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// Current state of the peer.
    pub state: PeerState,
    /// Whether this is a full node (vs light node).
    pub is_full_node: bool,
    /// When the peer was first seen (this session).
    pub first_seen: Instant,
    /// When the state last changed.
    pub last_state_change: Instant,
    /// Ban reason if state is Banned.
    pub ban_reason: Option<String>,
}

impl PeerInfo {
    /// Create a new peer info in Known state.
    pub fn new_known() -> Self {
        let now = Instant::now();
        Self {
            state: PeerState::Known,
            is_full_node: false,
            first_seen: now,
            last_state_change: now,
            ban_reason: None,
        }
    }

    /// Create a new peer info for a connected peer.
    pub fn new_connected(is_full_node: bool) -> Self {
        let now = Instant::now();
        Self {
            state: PeerState::Connected,
            is_full_node,
            first_seen: now,
            last_state_change: now,
            ban_reason: None,
        }
    }

    /// Transition to a new state.
    pub fn transition_to(&mut self, new_state: PeerState) {
        self.state = new_state;
        self.last_state_change = Instant::now();
    }

    /// Mark as banned with optional reason.
    pub fn ban(&mut self, reason: Option<String>) {
        self.state = PeerState::Banned;
        self.last_state_change = Instant::now();
        self.ban_reason = reason;
    }
}

/// Persistable peer data including the full BzzAddress.
///
/// This contains everything needed to:
/// 1. Reconnect to the peer (multiaddrs)
/// 2. Verify peer identity (signature, nonce)
/// 3. Broadcast peer info via Hive protocol
/// 4. Identify for settlements (ethereum_address)
/// 5. Track peer reputation (score)
///
/// Unlike [`PeerInfo`], this is designed for disk persistence and
/// can be serialized/deserialized.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredPeer {
    /// The peer's identity (overlay, multiaddrs, signature, nonce, ethereum_address).
    pub peer: SwarmPeer,
    /// Whether this is a full node.
    pub is_full_node: bool,
    /// Unix timestamp (seconds) when first seen.
    pub first_seen_unix: u64,
    /// Unix timestamp (seconds) when last successfully connected.
    pub last_connected_unix: Option<u64>,
    /// If banned, the reason and timestamp.
    pub ban_info: Option<BanInfo>,
    /// Peer reputation score snapshot.
    pub score: PeerScoreSnapshot,
}

/// Information about a banned peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BanInfo {
    /// Unix timestamp when banned.
    pub banned_at_unix: u64,
    /// Optional reason for the ban.
    pub reason: Option<String>,
}

impl StoredPeer {
    /// Create a new stored peer from a SwarmPeer.
    pub fn new(peer: SwarmPeer, is_full_node: bool) -> Self {
        Self {
            peer,
            is_full_node,
            first_seen_unix: current_unix_timestamp(),
            last_connected_unix: None,
            ban_info: None,
            score: PeerScoreSnapshot::new(),
        }
    }

    /// Create a new stored peer from BzzAddress components (convenience method).
    ///
    /// This creates a `SwarmPeer` from pre-validated data and wraps it.
    pub fn from_components(
        overlay: B256,
        multiaddrs: Vec<Multiaddr>,
        signature: alloy_primitives::Signature,
        nonce: B256,
        ethereum_address: alloy_primitives::Address,
        is_full_node: bool,
    ) -> Self {
        let peer =
            SwarmPeer::from_validated(multiaddrs, signature, overlay, nonce, ethereum_address);
        Self::new(peer, is_full_node)
    }

    /// Get the overlay address as B256.
    pub fn overlay(&self) -> B256 {
        B256::from_slice(self.peer.overlay().as_ref())
    }

    /// Get the multiaddrs.
    pub fn multiaddrs(&self) -> &[Multiaddr] {
        self.peer.multiaddrs()
    }

    /// Get the signature.
    pub fn signature(&self) -> &alloy_primitives::Signature {
        self.peer.signature()
    }

    /// Get the nonce.
    pub fn nonce(&self) -> &B256 {
        self.peer.nonce()
    }

    /// Get the ethereum address.
    pub fn ethereum_address(&self) -> &alloy_primitives::Address {
        self.peer.ethereum_address()
    }

    /// Get a reference to the underlying SwarmPeer.
    pub fn swarm_peer(&self) -> &SwarmPeer {
        &self.peer
    }

    /// Record a successful connection.
    pub fn record_connection(&mut self) {
        self.last_connected_unix = Some(current_unix_timestamp());
        self.score.record_connection_success();
    }

    /// Record a connection timeout.
    pub fn record_timeout(&mut self) {
        self.score.record_connection_timeout();
    }

    /// Record a connection refusal.
    pub fn record_refused(&mut self) {
        self.score.record_connection_refused();
    }

    /// Record a handshake failure.
    pub fn record_handshake_failure(&mut self) {
        self.score.record_handshake_failure();
    }

    /// Record a protocol error.
    pub fn record_protocol_error(&mut self) {
        self.score.record_protocol_error();
    }

    /// Get the peer's score.
    pub fn score_value(&self) -> f64 {
        self.score.score
    }

    /// Get the connection success count.
    pub fn connection_count(&self) -> u32 {
        self.score.connection_successes
    }

    /// Get the total failure count.
    pub fn failure_count(&self) -> u32 {
        self.score.connection_timeouts
            + self.score.connection_refusals
            + self.score.handshake_failures
    }

    /// Ban this peer.
    pub fn ban(&mut self, reason: Option<String>) {
        self.ban_info = Some(BanInfo {
            banned_at_unix: current_unix_timestamp(),
            reason,
        });
    }

    /// Unban this peer.
    pub fn unban(&mut self) {
        self.ban_info = None;
    }

    /// Check if this peer is banned.
    pub fn is_banned(&self) -> bool {
        self.ban_info.is_some()
    }

    /// Update multiaddrs (e.g., when peer announces new addresses).
    ///
    /// This creates a new SwarmPeer with updated multiaddrs while preserving
    /// the other identity fields (signature, overlay, nonce, ethereum_address).
    pub fn update_multiaddrs(&mut self, multiaddrs: Vec<Multiaddr>) {
        self.peer = SwarmPeer::from_validated(
            multiaddrs,
            self.peer.signature().clone(),
            B256::from_slice(self.peer.overlay().as_ref()),
            *self.peer.nonce(),
            *self.peer.ethereum_address(),
        );
    }
}

/// Get current Unix timestamp in seconds.
fn current_unix_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
