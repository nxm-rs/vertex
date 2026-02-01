//! Bootnode connection management.
//!
//! Manages initial network entry by connecting to bootstrap nodes.
//! For `/dnsaddr/` resolution, use the [`dns`](crate::dns) module.

use libp2p::Multiaddr;
use rand::seq::SliceRandom;

const MAX_BOOTNODE_ATTEMPTS: usize = 6;
const MIN_BOOTNODE_CONNECTIONS: usize = 1;

#[allow(dead_code)]
const BOOTNODE_CONNECT_TIMEOUT_SECS: u64 = 15;

/// Bootnode connection strategy.
#[derive(Debug, Clone)]
pub struct BootnodeConnector {
    bootnodes: Vec<Multiaddr>,
    max_attempts: usize,
    min_connections: usize,
}

impl BootnodeConnector {
    /// Create a new connector with the given bootnodes.
    pub fn new(bootnodes: Vec<Multiaddr>) -> Self {
        Self {
            bootnodes,
            max_attempts: MAX_BOOTNODE_ATTEMPTS,
            min_connections: MIN_BOOTNODE_CONNECTIONS,
        }
    }

    /// Set maximum connection attempts per bootnode.
    pub fn with_max_attempts(mut self, attempts: usize) -> Self {
        self.max_attempts = attempts;
        self
    }

    /// Set minimum successful connections required.
    pub fn with_min_connections(mut self, min: usize) -> Self {
        self.min_connections = min;
        self
    }

    /// Get configured bootnodes.
    pub fn bootnodes(&self) -> &[Multiaddr] {
        &self.bootnodes
    }

    /// Get maximum attempts per bootnode.
    pub fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    /// Get minimum connections required.
    pub fn min_connections(&self) -> usize {
        self.min_connections
    }

    /// Get bootnodes in random order for load distribution.
    pub fn shuffled_bootnodes(&self) -> Vec<Multiaddr> {
        Self::shuffle(&self.bootnodes)
    }

    /// Shuffle addresses randomly.
    pub fn shuffle(addrs: &[Multiaddr]) -> Vec<Multiaddr> {
        let mut shuffled = addrs.to_vec();
        let mut rng = rand::rng();
        shuffled.shuffle(&mut rng);
        shuffled
    }

    /// Check if any bootnodes are configured.
    pub fn has_bootnodes(&self) -> bool {
        !self.bootnodes.is_empty()
    }

    /// Get bootnode count.
    pub fn bootnode_count(&self) -> usize {
        self.bootnodes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shuffled_bootnodes() {
        let bootnodes = vec![
            "/ip4/1.1.1.1/tcp/1634".parse().unwrap(),
            "/ip4/2.2.2.2/tcp/1634".parse().unwrap(),
            "/ip4/3.3.3.3/tcp/1634".parse().unwrap(),
        ];
        let connector = BootnodeConnector::new(bootnodes.clone());

        let shuffled = connector.shuffled_bootnodes();
        assert_eq!(shuffled.len(), bootnodes.len());

        for bn in &bootnodes {
            assert!(shuffled.contains(bn));
        }
    }

    #[test]
    fn test_shuffle_static() {
        let addrs: Vec<Multiaddr> = vec![
            "/ip4/1.1.1.1/tcp/1634".parse().unwrap(),
            "/ip4/2.2.2.2/tcp/1634".parse().unwrap(),
            "/ip4/3.3.3.3/tcp/1634".parse().unwrap(),
        ];

        let shuffled = BootnodeConnector::shuffle(&addrs);
        assert_eq!(shuffled.len(), addrs.len());

        for addr in &addrs {
            assert!(shuffled.contains(addr));
        }
    }

    #[test]
    fn test_builder_pattern() {
        let connector = BootnodeConnector::new(vec![])
            .with_max_attempts(10)
            .with_min_connections(5);

        assert_eq!(connector.max_attempts(), 10);
        assert_eq!(connector.min_connections(), 5);
    }
}
