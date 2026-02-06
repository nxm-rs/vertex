//! Kademlia routing configuration.

use std::time::Duration;

/// Target peers per bin before considering it saturated.
pub const DEFAULT_SATURATION_PEERS: usize = 8;

/// Maximum full nodes per bin (high water mark).
pub const DEFAULT_HIGH_WATERMARK: usize = 16;

/// Slots reserved for client (light) nodes per bin.
pub const DEFAULT_CLIENT_RESERVED_SLOTS: usize = 2;

/// Minimum peers in a bin for depth calculation (low water mark).
pub const DEFAULT_LOW_WATERMARK: usize = 3;

/// Interval for the manage loop to evaluate connections.
pub const DEFAULT_MANAGE_INTERVAL: Duration = Duration::from_secs(15);

/// Maximum connection attempts before removing a peer from known_peers.
pub const DEFAULT_MAX_CONNECT_ATTEMPTS: usize = 4;

/// Maximum connection attempts for neighbors (they get more tries).
pub const DEFAULT_MAX_NEIGHBOR_ATTEMPTS: usize = 6;

/// Maximum concurrent pending connections for neighbor (depth) bins.
pub const DEFAULT_MAX_NEIGHBOR_CANDIDATES: usize = 16;

/// Maximum concurrent pending connections for balanced (non-depth) bins.
pub const DEFAULT_MAX_BALANCED_CANDIDATES: usize = 16;

/// Configuration for Kademlia routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KademliaConfig {
    /// Target number of peers per bin before considering it saturated.
    pub saturation_peers: usize,
    /// Maximum full nodes per bin (high water mark).
    pub high_watermark: usize,
    /// Slots reserved for client (light) nodes per bin.
    pub client_reserved_slots: usize,
    /// Minimum peers required in a bin for depth calculation.
    pub low_watermark: usize,
    /// Maximum failed connection attempts before removing a peer.
    pub max_connect_attempts: usize,
    /// Maximum failed connection attempts for neighbor peers.
    pub max_neighbor_attempts: usize,
    /// Maximum concurrent pending connection candidates for neighbor (depth) bins.
    pub max_neighbor_candidates: usize,
    /// Maximum concurrent pending connection candidates for balanced (non-depth) bins.
    pub max_balanced_candidates: usize,
}

impl KademliaConfig {
    /// Create a default configuration in const context.
    pub const fn default_const() -> Self {
        Self {
            saturation_peers: DEFAULT_SATURATION_PEERS,
            high_watermark: DEFAULT_HIGH_WATERMARK,
            client_reserved_slots: DEFAULT_CLIENT_RESERVED_SLOTS,
            low_watermark: DEFAULT_LOW_WATERMARK,
            max_connect_attempts: DEFAULT_MAX_CONNECT_ATTEMPTS,
            max_neighbor_attempts: DEFAULT_MAX_NEIGHBOR_ATTEMPTS,
            max_neighbor_candidates: DEFAULT_MAX_NEIGHBOR_CANDIDATES,
            max_balanced_candidates: DEFAULT_MAX_BALANCED_CANDIDATES,
        }
    }

    /// Total maximum peers per bin (high watermark + client reserved).
    pub fn max_peers_per_bin(&self) -> usize {
        self.high_watermark + self.client_reserved_slots
    }

    pub fn with_saturation_peers(mut self, count: usize) -> Self {
        self.saturation_peers = count;
        self
    }

    pub fn with_high_watermark(mut self, count: usize) -> Self {
        self.high_watermark = count;
        self
    }

    pub fn with_client_reserved_slots(mut self, count: usize) -> Self {
        self.client_reserved_slots = count;
        self
    }

    pub fn with_low_watermark(mut self, count: usize) -> Self {
        self.low_watermark = count;
        self
    }

    pub fn with_max_connect_attempts(mut self, attempts: usize) -> Self {
        self.max_connect_attempts = attempts;
        self
    }
}

impl Default for KademliaConfig {
    fn default() -> Self {
        Self {
            saturation_peers: DEFAULT_SATURATION_PEERS,
            high_watermark: DEFAULT_HIGH_WATERMARK,
            client_reserved_slots: DEFAULT_CLIENT_RESERVED_SLOTS,
            low_watermark: DEFAULT_LOW_WATERMARK,
            max_connect_attempts: DEFAULT_MAX_CONNECT_ATTEMPTS,
            max_neighbor_attempts: DEFAULT_MAX_NEIGHBOR_ATTEMPTS,
            max_neighbor_candidates: DEFAULT_MAX_NEIGHBOR_CANDIDATES,
            max_balanced_candidates: DEFAULT_MAX_BALANCED_CANDIDATES,
        }
    }
}
