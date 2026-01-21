//! Bootnode connection and DNS address resolution.
//!
//! This module handles initial network entry by connecting to bootstrap nodes.
//! It supports both direct multiaddrs and `/dnsaddr/*` multiaddrs.
//!
//! # DNS Address Resolution
//!
//! Swarm uses `/dnsaddr/` multiaddrs for bootnodes (e.g., `/dnsaddr/mainnet.ethswarm.org`).
//! Resolution is handled automatically by libp2p's DNS transport when dialing.
//!
//! When libp2p's `dns::tokio::Transport` wraps the TCP transport, it:
//! 1. Intercepts dial requests containing `/dnsaddr/`, `/dns/`, `/dns4/`, `/dns6/`
//! 2. Queries DNS TXT records for `_dnsaddr.{domain}`
//! 3. Parses `dnsaddr=<multiaddr>` entries from TXT records
//! 4. Dials the resolved concrete addresses
//!
//! This means we can pass `/dnsaddr/mainnet.ethswarm.org` directly to libp2p
//! and it handles resolution transparently.
//!
//! # Connection Strategy
//!
//! - Attempt to connect to bootnodes with timeout
//! - Shuffle bootnode order to distribute load
//! - Stop after connecting to a minimum number of bootnodes (typically 3)
//! - Retry failed connections with backoff

use libp2p::multiaddr::Protocol;
use libp2p::Multiaddr;
use rand::seq::SliceRandom;

/// Maximum number of bootnode connection attempts per bootnode.
pub const MAX_BOOTNODE_ATTEMPTS: usize = 6;

/// Minimum number of bootnodes to connect to before considering bootstrap complete.
pub const MIN_BOOTNODE_CONNECTIONS: usize = 3;

/// Timeout for individual bootnode connection attempts in seconds.
pub const BOOTNODE_CONNECT_TIMEOUT_SECS: u64 = 15;

/// Handles bootnode connection management.
///
/// Note: DNS resolution for `/dnsaddr/` multiaddrs is handled automatically
/// by libp2p's DNS transport. This struct manages the connection strategy
/// and bootnode selection, not DNS resolution.
#[derive(Debug, Clone)]
pub struct BootnodeConnector {
    /// Bootnode multiaddrs (may include dnsaddr - resolved by libp2p).
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

    /// Check if a multiaddr uses DNS protocols.
    ///
    /// These addresses will be resolved automatically by libp2p's DNS transport
    /// when dialing. This method is useful for logging/debugging.
    pub fn is_dns_addr(addr: &Multiaddr) -> bool {
        addr.iter().any(|p| {
            matches!(
                p,
                Protocol::Dns(_)
                    | Protocol::Dns4(_)
                    | Protocol::Dns6(_)
                    | Protocol::Dnsaddr(_)
            )
        })
    }

    /// Get bootnodes shuffled randomly to distribute load.
    ///
    /// Call this when starting bootnode connections to avoid all nodes
    /// connecting to the same bootnode first.
    pub fn shuffled_bootnodes(&self) -> Vec<Multiaddr> {
        let mut bootnodes = self.bootnodes.clone();
        let mut rng = rand::rng();
        bootnodes.shuffle(&mut rng);
        bootnodes
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
    fn test_is_dns_addr_direct_ip() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        assert!(!BootnodeConnector::is_dns_addr(&addr));
    }

    #[test]
    fn test_is_dns_addr_dnsaddr() {
        let addr: Multiaddr = "/dnsaddr/mainnet.ethswarm.org".parse().unwrap();
        assert!(BootnodeConnector::is_dns_addr(&addr));
    }

    #[test]
    fn test_is_dns_addr_dns4() {
        let addr: Multiaddr = "/dns4/example.com/tcp/1634".parse().unwrap();
        assert!(BootnodeConnector::is_dns_addr(&addr));
    }

    #[test]
    fn test_shuffled_bootnodes() {
        let bootnodes = vec![
            "/ip4/1.1.1.1/tcp/1634".parse().unwrap(),
            "/ip4/2.2.2.2/tcp/1634".parse().unwrap(),
            "/ip4/3.3.3.3/tcp/1634".parse().unwrap(),
        ];
        let connector = BootnodeConnector::new(bootnodes.clone());

        // Should return same count
        let shuffled = connector.shuffled_bootnodes();
        assert_eq!(shuffled.len(), bootnodes.len());

        // All original bootnodes should be present
        for bn in &bootnodes {
            assert!(shuffled.contains(bn));
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
