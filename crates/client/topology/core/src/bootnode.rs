//! Bootnode connection management.
//!
//! This module handles initial network entry by managing connections to bootstrap nodes.
//! For `/dnsaddr/` resolution, use the [`dns`](crate::dns) module.
//!
//! # Usage
//!
//! ```ignore
//! use vertex_topology_core::{BootnodeConnector, resolve_all_dnsaddrs};
//!
//! let connector = BootnodeConnector::new(bootnodes);
//!
//! // Resolve any /dnsaddr/ entries to concrete multiaddrs
//! let resolved = resolve_all_dnsaddrs(connector.bootnodes()).await;
//!
//! // Shuffle for load distribution
//! let shuffled = BootnodeConnector::shuffle(&resolved);
//! ```

use libp2p::Multiaddr;
use rand::seq::SliceRandom;

/// Maximum number of bootnode connection attempts per bootnode.
const MAX_BOOTNODE_ATTEMPTS: usize = 6;

/// Minimum number of bootnodes to connect to before considering bootstrap complete.
const MIN_BOOTNODE_CONNECTIONS: usize = 1;

/// Timeout for individual bootnode connection attempts in seconds.
const BOOTNODE_CONNECT_TIMEOUT_SECS: u64 = 15;

/// Handles bootnode connection management.
///
/// Manages bootnode selection and connection strategy.
/// For `/dnsaddr/` resolution, use the [`dns`](crate::dns) module.
#[derive(Debug, Clone)]
pub struct BootnodeConnector {
    /// Bootnode multiaddrs (may include dnsaddr).
    bootnodes: Vec<Multiaddr>,

    /// Maximum connection attempts per bootnode.
    max_attempts: usize,

    /// Minimum successful connections needed.
    min_connections: usize,
}

impl BootnodeConnector {
    /// Create a new bootnode connector.
    pub fn new(bootnodes: Vec<Multiaddr>) -> Self {
        Self {
            bootnodes,
            max_attempts: MAX_BOOTNODE_ATTEMPTS,
            min_connections: MIN_BOOTNODE_CONNECTIONS,
        }
    }

    /// Set the maximum connection attempts per bootnode.
    pub fn with_max_attempts(mut self, attempts: usize) -> Self {
        self.max_attempts = attempts;
        self
    }

    /// Set the minimum number of successful connections needed.
    pub fn with_min_connections(mut self, min: usize) -> Self {
        self.min_connections = min;
        self
    }

    /// Get the configured bootnodes.
    pub fn bootnodes(&self) -> &[Multiaddr] {
        &self.bootnodes
    }

    /// Get the maximum attempts per bootnode.
    pub fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    /// Get the minimum connections required.
    pub fn min_connections(&self) -> usize {
        self.min_connections
    }

    /// Get bootnodes shuffled randomly to distribute load.
    pub fn shuffled_bootnodes(&self) -> Vec<Multiaddr> {
        Self::shuffle(&self.bootnodes)
    }

    /// Shuffle a list of addresses randomly.
    ///
    /// Useful for distributing connection attempts across multiple resolved addresses.
    pub fn shuffle(addrs: &[Multiaddr]) -> Vec<Multiaddr> {
        let mut shuffled = addrs.to_vec();
        let mut rng = rand::rng();
        shuffled.shuffle(&mut rng);
        shuffled
    }

    /// Check if we have any bootnodes configured.
    pub fn has_bootnodes(&self) -> bool {
        !self.bootnodes.is_empty()
    }

    /// Get the count of configured bootnodes.
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
