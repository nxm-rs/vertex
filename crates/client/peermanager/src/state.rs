//! Peer state types.
//!
//! This module defines the core peer state management types:
//! - [`PeerState`]: Connection lifecycle state machine
//! - [`PeerInfo`]: Runtime peer information (in-memory)
//! - [`StoredPeer`]: Persistable peer data including BzzAddress

use alloy_primitives::{B256, Signature};
use libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use web_time::Instant;

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
/// 1. Reconnect to the peer (underlays)
/// 2. Verify peer identity (signature, nonce)
/// 3. Broadcast peer info via Hive protocol
///
/// Unlike [`PeerInfo`], this is designed for disk persistence and
/// can be serialized/deserialized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredPeer {
    /// The peer's overlay address (primary key), stored as raw bytes.
    pub overlay: B256,
    /// Network-level addresses for connecting to the peer (as strings).
    pub underlays: Vec<String>,
    /// Cryptographic signature proving ownership of overlay/underlay pair.
    pub signature: Signature,
    /// Nonce used in overlay address derivation.
    pub nonce: B256,
    /// Whether this is a full node.
    pub is_full_node: bool,
    /// Unix timestamp (seconds) when first seen.
    pub first_seen_unix: u64,
    /// Unix timestamp (seconds) when last successfully connected.
    pub last_connected_unix: Option<u64>,
    /// Number of successful connections.
    pub connection_count: u32,
    /// Number of failed connection attempts.
    pub failure_count: u32,
    /// If banned, the reason and timestamp.
    pub ban_info: Option<BanInfo>,
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
    /// Create a new stored peer from BzzAddress components.
    pub fn new(
        overlay: B256,
        underlays: Vec<Multiaddr>,
        signature: Signature,
        nonce: B256,
        is_full_node: bool,
    ) -> Self {
        Self {
            overlay,
            underlays: underlays.into_iter().map(|a| a.to_string()).collect(),
            signature,
            nonce,
            is_full_node,
            first_seen_unix: current_unix_timestamp(),
            last_connected_unix: None,
            connection_count: 0,
            failure_count: 0,
            ban_info: None,
        }
    }

    /// Get the underlays as Multiaddrs.
    ///
    /// Returns only valid multiaddrs, skipping any that fail to parse.
    pub fn underlays(&self) -> Vec<Multiaddr> {
        self.underlays
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect()
    }

    /// Record a successful connection.
    pub fn record_connection(&mut self) {
        self.last_connected_unix = Some(current_unix_timestamp());
        self.connection_count = self.connection_count.saturating_add(1);
    }

    /// Record a failed connection attempt.
    pub fn record_failure(&mut self) {
        self.failure_count = self.failure_count.saturating_add(1);
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

    /// Update underlays (e.g., when peer announces new addresses).
    pub fn update_underlays(&mut self, underlays: Vec<Multiaddr>) {
        self.underlays = underlays.into_iter().map(|a| a.to_string()).collect();
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
