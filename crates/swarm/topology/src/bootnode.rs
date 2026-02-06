//! Bootnode connection management.
//!
//! Manages initial network entry by connecting to bootstrap nodes.
//! For `/dnsaddr/` resolution, use the [`dns`](crate::dns) module.

use libp2p::Multiaddr;
use rand::seq::SliceRandom;

const MIN_BOOTNODE_CONNECTIONS: usize = 1;

/// Bootnode connection strategy.
#[derive(Debug, Clone)]
pub(crate) struct BootnodeConnector {
    bootnodes: Vec<Multiaddr>,
    min_connections: usize,
}

impl Default for BootnodeConnector {
    fn default() -> Self {
        Self {
            bootnodes: Vec::new(),
            min_connections: MIN_BOOTNODE_CONNECTIONS,
        }
    }
}

impl BootnodeConnector {
    pub(crate) fn new(bootnodes: Vec<Multiaddr>) -> Self {
        Self {
            bootnodes,
            min_connections: MIN_BOOTNODE_CONNECTIONS,
        }
    }

    /// Get bootnodes in random order for load distribution.
    pub(crate) fn shuffled_bootnodes(&self) -> Vec<Multiaddr> {
        Self::shuffle(&self.bootnodes)
    }

    /// Shuffle addresses randomly.
    pub(crate) fn shuffle(addrs: &[Multiaddr]) -> Vec<Multiaddr> {
        let mut shuffled = addrs.to_vec();
        let mut rng = rand::rng();
        shuffled.shuffle(&mut rng);
        shuffled
    }

    /// Check if any bootnodes are configured.
    pub(crate) fn has_bootnodes(&self) -> bool {
        !self.bootnodes.is_empty()
    }

    /// Get minimum connections required.
    pub(crate) fn min_connections(&self) -> usize {
        self.min_connections
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
        let connector = BootnodeConnector::new(vec![]);
        assert_eq!(connector.min_connections(), MIN_BOOTNODE_CONNECTIONS);
    }
}
