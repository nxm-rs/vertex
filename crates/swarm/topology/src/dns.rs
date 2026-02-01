//! DNS resolution for `/dnsaddr/` multiaddrs.
//!
//! libp2p's DNS transport only returns one TXT record for dnsaddr. This module
//! resolves ALL records, enabling load distribution and failover across bootnodes.

use std::collections::HashSet;

use hickory_resolver::TokioResolver;
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;
use tracing::{debug, warn};

const MAX_DNS_RECURSION_DEPTH: usize = 10;

/// Errors from dnsaddr resolution.
#[derive(Debug, thiserror::Error)]
pub enum DnsaddrResolveError {
    #[error("DNS lookup failed: {0}")]
    DnsLookup(String),

    #[error("maximum DNS recursion depth exceeded")]
    MaxRecursionDepth,
}

/// Resolve a `/dnsaddr/` multiaddr to concrete addresses.
///
/// Non-dnsaddr inputs are returned unchanged.
pub async fn resolve_dnsaddr(addr: &Multiaddr) -> Result<Vec<Multiaddr>, DnsaddrResolveError> {
    let mut seen = HashSet::new();
    resolve_dnsaddr_with_seen(addr, &mut seen).await
}

/// Resolve with a seen set to prevent loops.
pub async fn resolve_dnsaddr_with_seen(
    addr: &Multiaddr,
    seen: &mut HashSet<String>,
) -> Result<Vec<Multiaddr>, DnsaddrResolveError> {
    resolve_recursive(addr, seen, 0).await
}

/// Resolve multiple multiaddrs. Non-dnsaddr pass through; failures include original.
pub async fn resolve_all_dnsaddrs(addrs: impl IntoIterator<Item = &Multiaddr>) -> Vec<Multiaddr> {
    let mut resolved = Vec::new();
    let mut seen = HashSet::new();

    for addr in addrs {
        if !is_dnsaddr(addr) {
            resolved.push(addr.clone());
            continue;
        }

        match resolve_dnsaddr_with_seen(addr, &mut seen).await {
            Ok(addrs) => {
                debug!(addr = %addr, resolved_count = addrs.len(), "Resolved dnsaddr");
                resolved.extend(addrs);
            }
            Err(e) => {
                warn!(addr = %addr, error = %e, "Failed to resolve dnsaddr");
                resolved.push(addr.clone());
            }
        }
    }

    resolved
}

/// Check if a multiaddr is a `/dnsaddr/` address.
pub fn is_dnsaddr(addr: &Multiaddr) -> bool {
    addr.iter().any(|p| matches!(p, Protocol::Dnsaddr(_)))
}

/// Extract domain from `/dnsaddr/{domain}`.
pub fn extract_dnsaddr_domain(addr: &Multiaddr) -> Option<String> {
    for proto in addr.iter() {
        if let Protocol::Dnsaddr(domain) = proto {
            return Some(domain.to_string());
        }
    }
    None
}

fn resolve_recursive<'a>(
    addr: &'a Multiaddr,
    seen: &'a mut HashSet<String>,
    depth: usize,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<Multiaddr>, DnsaddrResolveError>> + Send + 'a>,
> {
    Box::pin(async move {
        if depth > MAX_DNS_RECURSION_DEPTH {
            return Err(DnsaddrResolveError::MaxRecursionDepth);
        }

        let domain = match extract_dnsaddr_domain(addr) {
            Some(d) => d,
            None => return Ok(vec![addr.clone()]),
        };

        let cache_key = format!("_dnsaddr.{}", domain);
        if seen.contains(&cache_key) {
            debug!(domain = %domain, "Skipping already-seen dnsaddr domain");
            return Ok(vec![]);
        }
        seen.insert(cache_key.clone());

        let resolver = TokioResolver::tokio(ResolverConfig::default(), ResolverOpts::default());

        let txt_name = format!("_dnsaddr.{}", domain);
        debug!(name = %txt_name, "Querying DNS TXT records");

        let txt_records = resolver.txt_lookup(&txt_name).await.map_err(|e| {
            DnsaddrResolveError::DnsLookup(format!("Failed to lookup {}: {}", txt_name, e))
        })?;

        let mut results = Vec::new();

        for record in txt_records.iter() {
            for txt in record.txt_data() {
                let txt_str = String::from_utf8_lossy(txt);

                if let Some(value) = txt_str.strip_prefix("dnsaddr=") {
                    debug!(record = %value, "Found dnsaddr TXT record");

                    match value.parse::<Multiaddr>() {
                        Ok(resolved_addr) => {
                            let nested = resolve_recursive(&resolved_addr, seen, depth + 1).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_dnsaddr_false_for_ip() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        assert!(!is_dnsaddr(&addr));
    }

    #[test]
    fn test_is_dnsaddr_true() {
        let addr: Multiaddr = "/dnsaddr/mainnet.ethswarm.org".parse().unwrap();
        assert!(is_dnsaddr(&addr));
    }

    #[test]
    fn test_is_dnsaddr_false_for_dns4() {
        let addr: Multiaddr = "/dns4/example.com/tcp/1634".parse().unwrap();
        assert!(!is_dnsaddr(&addr));
    }

    #[test]
    fn test_extract_dnsaddr_domain() {
        let addr: Multiaddr = "/dnsaddr/mainnet.ethswarm.org".parse().unwrap();
        assert_eq!(
            extract_dnsaddr_domain(&addr),
            Some("mainnet.ethswarm.org".to_string())
        );

        let ip_addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        assert_eq!(extract_dnsaddr_domain(&ip_addr), None);
    }

    #[tokio::test]
    async fn test_resolve_non_dnsaddr_returns_as_is() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        let resolved = resolve_dnsaddr(&addr).await.unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0], addr);
    }
}
