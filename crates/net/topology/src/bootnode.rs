//! Bootnode connection and DNS address resolution.
//!
//! This module handles initial network entry by connecting to bootstrap nodes.
//! It supports both direct multiaddrs and `/dnsaddr/*` multiaddrs.
//!
//! # DNS Address Resolution
//!
//! Swarm uses `/dnsaddr/` multiaddrs for bootnodes (e.g., `/dnsaddr/mainnet.ethswarm.org`).
//! These addresses use a hierarchical DNS structure:
//!
//! ```text
//! /dnsaddr/mainnet.ethswarm.org
//!   └─> TXT _dnsaddr.mainnet.ethswarm.org = "dnsaddr=/dnsaddr/emea.mainnet.ethswarm.org"
//!         └─> TXT _dnsaddr.emea.mainnet.ethswarm.org = "dnsaddr=/dnsaddr/hel.mainnet.ethswarm.org"
//!             └─> TXT _dnsaddr.hel.mainnet.ethswarm.org = "dnsaddr=/ip4/.../tcp/1634/p2p/Qm..."
//! ```
//!
//! The `resolve_dnsaddr` function resolves these recursively to concrete addresses.
//!
//! # Connection Strategy
//!
//! - Resolve all `/dnsaddr/` entries to concrete multiaddrs
//! - Shuffle bootnode order to distribute load
//! - Try connecting to each bootnode until we get peers from hive
//! - Stop after connecting to a minimum number of bootnodes (typically 3)
//! - Retry failed connections with backoff

use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::TokioResolver;
use libp2p::multiaddr::Protocol;
use libp2p::Multiaddr;
use rand::seq::SliceRandom;
use std::collections::HashSet;
use tracing::{debug, warn};

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

    /// Resolve all bootnodes, expanding `/dnsaddr/` entries to concrete addresses.
    ///
    /// This recursively resolves DNS TXT records until we have concrete
    /// IP-based multiaddrs that can be dialed directly.
    ///
    /// Returns a list of resolved addresses, shuffled randomly.
    pub async fn resolve_all(&self) -> Vec<Multiaddr> {
        let mut resolved = Vec::new();
        let mut seen = HashSet::new();

        for bootnode in &self.bootnodes {
            match resolve_dnsaddr(bootnode, &mut seen).await {
                Ok(addrs) => {
                    debug!(
                        bootnode = %bootnode,
                        resolved_count = addrs.len(),
                        "Resolved bootnode addresses"
                    );
                    resolved.extend(addrs);
                }
                Err(e) => {
                    warn!(bootnode = %bootnode, error = %e, "Failed to resolve bootnode");
                    // If resolution fails, try the original address anyway
                    // (libp2p may be able to resolve it)
                    resolved.push(bootnode.clone());
                }
            }
        }

        // Shuffle to distribute load
        let mut rng = rand::rng();
        resolved.shuffle(&mut rng);

        resolved
    }
}

/// Maximum recursion depth for DNS resolution to prevent infinite loops.
const MAX_DNS_RECURSION_DEPTH: usize = 10;

/// Resolve a `/dnsaddr/` multiaddr to concrete addresses.
///
/// This function:
/// 1. Extracts the domain from the dnsaddr
/// 2. Queries DNS TXT records for `_dnsaddr.{domain}`
/// 3. Parses `dnsaddr=<multiaddr>` entries
/// 4. Recursively resolves any nested dnsaddr entries
///
/// Returns concrete (non-dnsaddr) multiaddrs that can be dialed.
pub async fn resolve_dnsaddr(
    addr: &Multiaddr,
    seen: &mut HashSet<String>,
) -> Result<Vec<Multiaddr>, DnsResolveError> {
    resolve_dnsaddr_recursive(addr, seen, 0).await
}

fn resolve_dnsaddr_recursive<'a>(
    addr: &'a Multiaddr,
    seen: &'a mut HashSet<String>,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<Multiaddr>, DnsResolveError>> + Send + 'a>>
{
    Box::pin(async move {
        if depth > MAX_DNS_RECURSION_DEPTH {
            return Err(DnsResolveError::MaxRecursionDepth);
        }

        // Check if this is a dnsaddr
        let domain = match extract_dnsaddr_domain(addr) {
            Some(d) => d,
            None => {
                // Not a dnsaddr, return as-is
                return Ok(vec![addr.clone()]);
            }
        };

        // Prevent infinite loops
        let cache_key = format!("_dnsaddr.{}", domain);
        if seen.contains(&cache_key) {
            return Ok(vec![]);
        }
        seen.insert(cache_key.clone());

        // Create DNS resolver
        let resolver = TokioResolver::tokio(ResolverConfig::default(), ResolverOpts::default());

        // Query TXT records
        let txt_name = format!("_dnsaddr.{}", domain);
        debug!(name = %txt_name, "Querying DNS TXT records");

        let txt_records = resolver.txt_lookup(&txt_name).await.map_err(|e| {
            DnsResolveError::DnsLookup(format!("Failed to lookup {}: {}", txt_name, e))
        })?;

        let mut results = Vec::new();

        for record in txt_records.iter() {
            for txt in record.txt_data() {
                let txt_str = String::from_utf8_lossy(txt);

                // Parse dnsaddr=<multiaddr> format
                if let Some(value) = txt_str.strip_prefix("dnsaddr=") {
                    debug!(record = %value, "Found dnsaddr TXT record");

                    match value.parse::<Multiaddr>() {
                        Ok(resolved_addr) => {
                            // Recursively resolve if this is also a dnsaddr
                            let nested =
                                resolve_dnsaddr_recursive(&resolved_addr, seen, depth + 1).await?;
                            results.extend(nested);
                        }
                        Err(e) => {
                            warn!(value = %value, error = %e, "Failed to parse multiaddr from TXT record");
                        }
                    }
                }
            }
        }

        Ok(results)
    })
}

/// Extract the domain from a `/dnsaddr/{domain}` multiaddr.
fn extract_dnsaddr_domain(addr: &Multiaddr) -> Option<String> {
    for proto in addr.iter() {
        if let Protocol::Dnsaddr(domain) = proto {
            return Some(domain.to_string());
        }
    }
    None
}

/// Errors that can occur during DNS resolution.
#[derive(Debug, thiserror::Error)]
pub enum DnsResolveError {
    /// DNS lookup failed
    #[error("DNS lookup failed: {0}")]
    DnsLookup(String),

    /// Maximum recursion depth exceeded
    #[error("Maximum DNS recursion depth exceeded")]
    MaxRecursionDepth,
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
