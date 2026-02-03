//! Kademlia topology configuration.
//!
//! # Water Mark Concepts
//!
//! The topology uses three thresholds per bin:
//!
//! - **Low watermark** (`low_watermark`, default 3): Minimum peers for a bin to
//!   contribute to depth calculation. Bins below this are considered "empty".
//!
//! - **Saturation** (`saturation_peers`, default 8): Target capacity. The manage
//!   loop stops actively seeking new connections once a bin reaches this level.
//!
//! - **High watermark** (`high_watermark`, default 16): Maximum capacity for full
//!   nodes. New inbound full node connections are rejected when a bin is at or
//!   above this level.
//!
//! Additionally, `client_reserved_slots` reserves space in each bin for light
//! (client/gateway) nodes, ensuring they can always connect even when bins are
//! near capacity.

use std::time::Duration;

/// Default target peers per bin before considering it saturated.
pub const DEFAULT_SATURATION_PEERS: usize = 8;

/// Default maximum full nodes per bin (high water mark).
pub const DEFAULT_HIGH_WATERMARK: usize = 16;

/// Default slots reserved for client (light) nodes per bin.
pub const DEFAULT_CLIENT_RESERVED_SLOTS: usize = 2;

/// Default minimum peers in a bin for depth calculation (low water mark).
pub const DEFAULT_LOW_WATERMARK: usize = 3;

/// Default interval for the manage loop to evaluate connections.
pub const DEFAULT_MANAGE_INTERVAL: Duration = Duration::from_secs(15);

/// Default maximum connection attempts before removing a peer from known_peers.
pub const DEFAULT_MAX_CONNECT_ATTEMPTS: usize = 4;

/// Default maximum connection attempts for neighbors (they get more tries).
pub const DEFAULT_MAX_NEIGHBOR_ATTEMPTS: usize = 6;

/// Default maximum concurrent pending connections.
pub const DEFAULT_MAX_PENDING_CONNECTIONS: usize = 16;

/// Configuration for Kademlia topology management.
#[derive(Debug, Clone)]
pub struct KademliaConfig {
    /// Target number of peers per bin before considering it saturated.
    /// The manage loop stops seeking new connections once this is reached.
    pub saturation_peers: usize,

    /// Maximum full nodes per bin (high water mark).
    /// New inbound full node connections are rejected above this level.
    pub high_watermark: usize,

    /// Slots reserved for client (light) nodes per bin.
    /// These slots are not counted against the high watermark for full nodes.
    pub client_reserved_slots: usize,

    /// Minimum peers required in a bin for depth calculation (low water mark).
    pub low_watermark: usize,

    /// Interval for the manage loop to check and adjust topology.
    pub manage_interval: Duration,

    /// Maximum failed connection attempts before removing a peer from known_peers.
    pub max_connect_attempts: usize,

    /// Maximum failed connection attempts for neighbor peers (they get more tries).
    pub max_neighbor_attempts: usize,

    /// Maximum concurrent pending connection attempts.
    pub max_pending_connections: usize,
}

impl KademliaConfig {
    /// Get the total maximum peers per bin (high watermark + client reserved).
    /// This is the absolute maximum before rejecting any connections.
    pub fn max_peers_per_bin(&self) -> usize {
        self.high_watermark + self.client_reserved_slots
    }
}

impl Default for KademliaConfig {
    fn default() -> Self {
        Self {
            saturation_peers: DEFAULT_SATURATION_PEERS,
            high_watermark: DEFAULT_HIGH_WATERMARK,
            client_reserved_slots: DEFAULT_CLIENT_RESERVED_SLOTS,
            low_watermark: DEFAULT_LOW_WATERMARK,
            manage_interval: DEFAULT_MANAGE_INTERVAL,
            max_connect_attempts: DEFAULT_MAX_CONNECT_ATTEMPTS,
            max_neighbor_attempts: DEFAULT_MAX_NEIGHBOR_ATTEMPTS,
            max_pending_connections: DEFAULT_MAX_PENDING_CONNECTIONS,
        }
    }
}

impl KademliaConfig {
    /// Set the saturation target per bin.
    pub fn with_saturation_peers(mut self, count: usize) -> Self {
        self.saturation_peers = count;
        self
    }

    /// Set the high watermark (max full nodes per bin).
    pub fn with_high_watermark(mut self, count: usize) -> Self {
        self.high_watermark = count;
        self
    }

    /// Set slots reserved for client nodes per bin.
    pub fn with_client_reserved_slots(mut self, count: usize) -> Self {
        self.client_reserved_slots = count;
        self
    }

    /// Set the low watermark for depth calculation.
    pub fn with_low_watermark(mut self, count: usize) -> Self {
        self.low_watermark = count;
        self
    }

    /// Set the manage loop interval.
    pub fn with_manage_interval(mut self, interval: Duration) -> Self {
        self.manage_interval = interval;
        self
    }
}
