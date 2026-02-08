//! Kademlia routing configuration.

const DEFAULT_SATURATION_PEERS: usize = 8;
const DEFAULT_HIGH_WATERMARK: usize = 16;
const DEFAULT_CLIENT_RESERVED_SLOTS: usize = 2;
const DEFAULT_LOW_WATERMARK: usize = 3;
const DEFAULT_MAX_CONNECT_ATTEMPTS: usize = 4;
const DEFAULT_MAX_NEIGHBOR_ATTEMPTS: usize = 6;
const DEFAULT_MAX_NEIGHBOR_CANDIDATES: usize = 16;
const DEFAULT_MAX_BALANCED_CANDIDATES: usize = 16;

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
    /// Total maximum peers per bin (high watermark + client reserved).
    pub(crate) fn max_peers_per_bin(&self) -> usize {
        self.high_watermark + self.client_reserved_slots
    }

    #[cfg(test)]
    pub(crate) fn with_high_watermark(mut self, count: usize) -> Self {
        self.high_watermark = count;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_low_watermark(mut self, count: usize) -> Self {
        self.low_watermark = count;
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
