//! Recursive `/dnsaddr/` multiaddr resolution (resolves ALL TXT records, unlike libp2p's DNS transport).

use std::collections::HashSet;

use hickory_resolver::Resolver;
use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;
use tracing::{debug, warn};

/// Maximum recursive dnsaddr depth (guards against CNAME-style loops).
const MAX_RECURSION_DEPTH: usize = 10;

/// Check whether a multiaddr contains a `/dnsaddr/` component.
#[must_use]
pub fn is_dnsaddr(addr: &Multiaddr) -> bool {
    addr.iter().any(|p| matches!(p, Protocol::Dnsaddr(_)))
}

/// Resolve a batch of multiaddrs, expanding every `/dnsaddr/` entry.
///
/// - Non-dnsaddr inputs pass through unchanged.
/// - A shared seen-set deduplicates across the whole batch.
/// - On resolution failure the original address is kept as fallback.
pub async fn resolve_all(addrs: impl IntoIterator<Item = &Multiaddr>) -> Vec<Multiaddr> {
    let mut resolved = Vec::new();
    let mut seen = HashSet::new();

    for addr in addrs {
        if !is_dnsaddr(addr) {
            resolved.push(addr.clone());
            continue;
        }

        match resolve_one(addr, &mut seen).await {
            Ok(addrs) => {
                debug!(addr = %addr, resolved_count = addrs.len(), "Resolved dnsaddr");
                resolved.extend(addrs);
            }
            Err(e) => {
                warn!(addr = %addr, error = %e, "Failed to resolve dnsaddr, keeping original");
                resolved.push(addr.clone());
            }
        }
    }

    resolved
}

/// Internal dnsaddr resolution errors.
#[derive(Debug, thiserror::Error)]
enum ResolveError {
    #[error("DNS lookup failed: {0}")]
    DnsLookup(String),

    #[error("maximum DNS recursion depth exceeded")]
    MaxRecursionDepth,
}

/// Resolve a single dnsaddr, reusing a shared `seen` set.
async fn resolve_one(
    addr: &Multiaddr,
    seen: &mut HashSet<String>,
) -> Result<Vec<Multiaddr>, ResolveError> {
    resolve_recursive(addr, seen, 0).await
}

/// Extract domain from the first `/dnsaddr/{domain}` component.
fn extract_domain(addr: &Multiaddr) -> Option<String> {
    addr.iter().find_map(|p| match p {
        Protocol::Dnsaddr(domain) => Some(domain.to_string()),
        _ => None,
    })
}

fn resolve_recursive<'a>(
    addr: &'a Multiaddr,
    seen: &'a mut HashSet<String>,
    depth: usize,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<Multiaddr>, ResolveError>> + Send + 'a>,
> {
    Box::pin(async move {
        if depth > MAX_RECURSION_DEPTH {
            return Err(ResolveError::MaxRecursionDepth);
        }

        let domain = match extract_domain(addr) {
            Some(d) => d,
            None => return Ok(vec![addr.clone()]),
        };

        let cache_key = format!("_dnsaddr.{}", domain);
        if seen.contains(&cache_key) {
            debug!(domain = %domain, "Skipping already-seen dnsaddr domain");
            return Ok(vec![]);
        }
        seen.insert(cache_key.clone());

        let resolver = Resolver::builder_tokio()
            .map_err(|e| ResolveError::DnsLookup(format!("failed to create resolver: {e}")))?
            .build();

        let txt_name = format!("_dnsaddr.{}", domain);
        debug!(name = %txt_name, "Querying DNS TXT records");

        let txt_records = resolver
            .txt_lookup(&txt_name)
            .await
            .map_err(|e| ResolveError::DnsLookup(format!("lookup {txt_name}: {e}")))?;

        let mut results = Vec::new();

        for record in txt_records.iter() {
            for txt in record.txt_data() {
                let txt_str = String::from_utf8_lossy(txt);

                if let Some(value) = txt_str.strip_prefix("dnsaddr=") {
                    debug!(record = %value, "Found dnsaddr TXT record");

                    match value.parse::<Multiaddr>() {
                        Ok(resolved_addr) => {
                            let nested =
                                resolve_recursive(&resolved_addr, seen, depth + 1).await?;
                            results.extend(nested);
                        }
                        Err(e) => {
                            warn!(
                                value = %value,
                                error = %e,
                                "Failed to parse multiaddr from TXT record"
                            );
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
    fn is_dnsaddr_false_for_ip() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        assert!(!is_dnsaddr(&addr));
    }

    #[test]
    fn is_dnsaddr_true() {
        let addr: Multiaddr = "/dnsaddr/mainnet.ethswarm.org".parse().unwrap();
        assert!(is_dnsaddr(&addr));
    }

    #[test]
    fn is_dnsaddr_false_for_dns4() {
        let addr: Multiaddr = "/dns4/example.com/tcp/1634".parse().unwrap();
        assert!(!is_dnsaddr(&addr));
    }

    #[test]
    fn extract_domain_from_dnsaddr() {
        let addr: Multiaddr = "/dnsaddr/mainnet.ethswarm.org".parse().unwrap();
        assert_eq!(extract_domain(&addr), Some("mainnet.ethswarm.org".to_string()));
    }

    #[test]
    fn extract_domain_returns_none_for_ip() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        assert_eq!(extract_domain(&addr), None);
    }

    #[tokio::test]
    async fn resolve_all_passes_non_dnsaddr_through() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        let resolved = resolve_all(&[addr.clone()]).await;
        assert_eq!(resolved, vec![addr]);
    }

    #[tokio::test]
    async fn resolve_one_returns_non_dnsaddr_unchanged() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        let mut seen = HashSet::new();
        let resolved = resolve_one(&addr, &mut seen).await.unwrap();
        assert_eq!(resolved, vec![addr]);
    }
}
