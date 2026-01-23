//! Kademlia topology configuration.

use std::time::Duration;

/// Default target peers per bin before considering it saturated.
pub const DEFAULT_SATURATION_PEERS: usize = 8;

/// Default maximum peers per bin before rejecting inbound connections.
pub const DEFAULT_OVERSATURATION_PEERS: usize = 18;

/// Default minimum peers in a bin for depth calculation.
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
    pub saturation_peers: usize,

    /// Maximum peers per bin before rejecting new inbound connections.
    pub oversaturation_peers: usize,

    /// Minimum peers required in a bin for it to contribute to depth calculation.
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

impl Default for KademliaConfig {
    fn default() -> Self {
        Self {
            saturation_peers: DEFAULT_SATURATION_PEERS,
            oversaturation_peers: DEFAULT_OVERSATURATION_PEERS,
            low_watermark: DEFAULT_LOW_WATERMARK,
            manage_interval: DEFAULT_MANAGE_INTERVAL,
            max_connect_attempts: DEFAULT_MAX_CONNECT_ATTEMPTS,
            max_neighbor_attempts: DEFAULT_MAX_NEIGHBOR_ATTEMPTS,
            max_pending_connections: DEFAULT_MAX_PENDING_CONNECTIONS,
        }
    }
}

impl KademliaConfig {
    /// Create a new config with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the saturation target per bin.
    pub fn with_saturation_peers(mut self, count: usize) -> Self {
        self.saturation_peers = count;
        self
    }

    /// Set the oversaturation threshold per bin.
    pub fn with_oversaturation_peers(mut self, count: usize) -> Self {
        self.oversaturation_peers = count;
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
