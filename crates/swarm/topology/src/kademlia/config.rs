//! Kademlia routing configuration.

use super::limits::DepthAwareLimits;

const DEFAULT_MAX_CONNECT_ATTEMPTS: usize = 4;
const DEFAULT_MAX_NEIGHBOR_ATTEMPTS: usize = 6;
const DEFAULT_MAX_NEIGHBOR_CANDIDATES: usize = 16;
const DEFAULT_MAX_BALANCED_CANDIDATES: usize = 16;

/// Configuration for Kademlia routing.
#[derive(Debug, Clone)]
pub struct KademliaConfig {
    /// Depth-aware per-bin capacity limits.
    pub limits: DepthAwareLimits,
    /// Maximum failed connection attempts before removing a peer.
    pub max_connect_attempts: usize,
    /// Maximum failed connection attempts for neighbor peers.
    pub max_neighbor_attempts: usize,
    /// Maximum concurrent pending candidates for neighbor (depth) bins.
    pub max_neighbor_candidates: usize,
    /// Maximum concurrent pending candidates for balanced (non-depth) bins.
    pub max_balanced_candidates: usize,
}

impl Default for KademliaConfig {
    fn default() -> Self {
        Self {
            limits: DepthAwareLimits::default(),
            max_connect_attempts: DEFAULT_MAX_CONNECT_ATTEMPTS,
            max_neighbor_attempts: DEFAULT_MAX_NEIGHBOR_ATTEMPTS,
            max_neighbor_candidates: DEFAULT_MAX_NEIGHBOR_CANDIDATES,
            max_balanced_candidates: DEFAULT_MAX_BALANCED_CANDIDATES,
        }
    }
}

impl KademliaConfig {
    /// Create with custom depth-aware limits.
    pub fn with_limits(limits: DepthAwareLimits) -> Self {
        Self { limits, ..Default::default() }
    }

    /// Create with custom total target peers.
    pub fn with_total_target(mut self, total: usize) -> Self {
        self.limits = DepthAwareLimits::new(total, self.limits.nominal());
        self
    }

    /// Create with custom nominal minimum per bin.
    pub fn with_nominal(mut self, nominal: usize) -> Self {
        self.limits = DepthAwareLimits::new(self.limits.total_target(), nominal);
        self
    }

    /// Create with custom inbound headroom.
    pub fn with_inbound_headroom(mut self, headroom: usize) -> Self {
        self.limits = self.limits.with_inbound_headroom(headroom);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = KademliaConfig::default();
        assert_eq!(config.limits.nominal(), 3);
        assert_eq!(config.limits.total_target(), 160);
        assert_eq!(config.max_connect_attempts, 4);
    }

    #[test]
    fn test_with_total_target() {
        let config = KademliaConfig::default().with_total_target(256);
        assert_eq!(config.limits.total_target(), 256);
        assert_eq!(config.limits.nominal(), 3);
    }

    #[test]
    fn test_with_nominal() {
        let config = KademliaConfig::default().with_nominal(5);
        assert_eq!(config.limits.nominal(), 5);
        assert_eq!(config.limits.total_target(), 160);
    }

    #[test]
    fn test_with_inbound_headroom() {
        let config = KademliaConfig::default().with_inbound_headroom(8);
        // Headroom is internal; verify depth-aware behavior works
        // At depth 8, bin 7 target = 35. With headroom 8, ceiling = 43.
        // At target + 7 = 42, should still accept inbound
        assert!(config.limits.should_accept_inbound(7, 8, 35 + 7));
        // At target + 8 = 43, should not accept
        assert!(!config.limits.should_accept_inbound(7, 8, 35 + 8));
    }

    #[test]
    fn test_with_limits() {
        let custom = DepthAwareLimits::new(200, 4);
        let config = KademliaConfig::with_limits(custom);
        assert_eq!(config.limits.total_target(), 200);
        assert_eq!(config.limits.nominal(), 4);
    }
}
