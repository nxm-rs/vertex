//! Recursive `/dnsaddr/` multiaddr resolution (resolves ALL TXT records, unlike libp2p's DNS transport).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use hickory_resolver::proto::rr::RData;
use hickory_resolver::{Resolver, TokioResolver};
use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;
use tracing::{debug, warn};

/// Maximum recursive dnsaddr depth (guards against CNAME-style loops).
const MAX_RECURSION_DEPTH: usize = 10;

/// Ceiling on `dnsaddr=` TXT records processed per batch resolution: the
/// breadth bound pairing the depth bound above, so a hostile or mis-authored
/// zone cannot fan out unboundedly. Also caps the resolved leaf count, since
/// every leaf comes from one record.
const MAX_DNSADDR_RECORDS: usize = 256;

/// Check whether a multiaddr contains a `/dnsaddr/` component.
#[must_use]
pub fn is_dnsaddr(addr: &Multiaddr) -> bool {
    addr.iter().any(|p| matches!(p, Protocol::Dnsaddr(_)))
}

/// Failure to construct the resolver from the system DNS configuration.
#[derive(Debug, thiserror::Error)]
#[error("failed to build system DNS resolver: {0}")]
pub struct ResolverError(#[from] hickory_resolver::net::NetError);

/// Outcome of a batch resolution.
#[derive(Debug, Clone)]
pub struct Resolution {
    /// Expanded multiaddrs; an entry that failed to resolve falls back to its
    /// original form.
    pub addrs: Vec<Multiaddr>,
    /// Time until the earliest consulted DNS record expires, for scheduling
    /// re-resolution. `None` when no lookup succeeded.
    pub min_ttl: Option<Duration>,
    /// Leaves dropped for lacking a `/p2p/` peer identity, which the dial
    /// path requires.
    pub invalid_leaves: usize,
    /// Whether resolution stopped early at the record-breadth cap.
    pub truncated: bool,
}

/// Mutable state threaded through one batch resolution: dedup, TTL floor,
/// and the breadth budget shared across every entry in the batch.
#[derive(Default)]
struct ResolveState {
    seen: HashSet<String>,
    min_ttl: Option<Duration>,
    records: usize,
    invalid_leaves: usize,
    truncated: bool,
}

/// Recursive `/dnsaddr/` resolver over one shared system resolver.
///
/// Build once and share (clones are cheap handles to the same resolver):
/// lookups are cached per record TTL across calls, so repeated resolution
/// honours DNS caching instead of re-querying every time.
#[derive(Clone)]
pub struct DnsaddrResolver {
    resolver: TokioResolver,
}

impl DnsaddrResolver {
    /// Build from the system DNS configuration.
    pub fn from_system_conf() -> Result<Self, ResolverError> {
        Ok(Self {
            resolver: Resolver::builder_tokio()?.build()?,
        })
    }

    /// Resolve a batch of multiaddrs, expanding every `/dnsaddr/` entry.
    ///
    /// - Non-dnsaddr inputs pass through unchanged and unvalidated.
    /// - A shared seen-set deduplicates across the whole batch.
    /// - Leaves lacking a `/p2p/` peer identity are dropped and counted.
    /// - On resolution failure the original address is kept as fallback.
    pub async fn resolve_all(&self, addrs: impl IntoIterator<Item = &Multiaddr>) -> Resolution {
        let mut resolved = Vec::new();
        let mut state = ResolveState::default();

        for addr in addrs {
            if !is_dnsaddr(addr) {
                resolved.push(addr.clone());
                continue;
            }

            match self.resolve_recursive(addr, &mut state, 0).await {
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

        Resolution {
            addrs: resolved,
            min_ttl: state.min_ttl,
            invalid_leaves: state.invalid_leaves,
            truncated: state.truncated,
        }
    }

    fn resolve_recursive<'a>(
        &'a self,
        addr: &'a Multiaddr,
        state: &'a mut ResolveState,
        depth: usize,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<Multiaddr>, ResolveError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if depth > MAX_RECURSION_DEPTH {
                return Err(ResolveError::MaxRecursionDepth);
            }

            let Some(domain) = extract_domain(addr) else {
                // A leaf out of a TXT record dials nowhere without a peer
                // identity: the dial path needs the /p2p/ component.
                if addr.iter().any(|p| matches!(p, Protocol::P2p(_))) {
                    return Ok(vec![addr.clone()]);
                }
                debug!(addr = %addr, "Dropping dnsaddr leaf without a /p2p/ component");
                state.invalid_leaves += 1;
                return Ok(vec![]);
            };

            if state.records >= MAX_DNSADDR_RECORDS {
                state.truncated = true;
                return Ok(vec![]);
            }

            let txt_name = format!("_dnsaddr.{}", domain);
            if state.seen.contains(&txt_name) {
                debug!(domain = %domain, "Skipping already-seen dnsaddr domain");
                return Ok(vec![]);
            }
            state.seen.insert(txt_name.clone());

            debug!(name = %txt_name, "Querying DNS TXT records");

            let txt_records = self
                .resolver
                .txt_lookup(&txt_name)
                .await
                .map_err(|e| ResolveError::DnsLookup(format!("lookup {txt_name}: {e}")))?;

            // The lookup's validity horizon is the remaining TTL, so a cached
            // response schedules the next re-resolution at the record's true
            // expiry rather than a full TTL from now.
            let ttl = txt_records
                .valid_until()
                .saturating_duration_since(Instant::now());
            state.min_ttl = Some(state.min_ttl.map_or(ttl, |current| current.min(ttl)));

            let mut results = Vec::new();

            for record in txt_records.answers() {
                let RData::TXT(txt) = &record.data else {
                    continue;
                };
                for bytes in txt.txt_data.iter() {
                    let txt_str = String::from_utf8_lossy(bytes);

                    if let Some(value) = txt_str.strip_prefix("dnsaddr=") {
                        if state.records >= MAX_DNSADDR_RECORDS {
                            warn!(
                                domain = %domain,
                                cap = MAX_DNSADDR_RECORDS,
                                "Stopping dnsaddr resolution at the record cap"
                            );
                            state.truncated = true;
                            return Ok(results);
                        }
                        state.records += 1;

                        debug!(record = %value, "Found dnsaddr TXT record");

                        match value.parse::<Multiaddr>() {
                            Ok(resolved_addr) => {
                                let nested = self
                                    .resolve_recursive(&resolved_addr, state, depth + 1)
                                    .await?;
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
}

/// Internal dnsaddr resolution errors.
#[derive(Debug, thiserror::Error)]
enum ResolveError {
    #[error("DNS lookup failed: {0}")]
    DnsLookup(String),

    #[error("maximum DNS recursion depth exceeded")]
    MaxRecursionDepth,
}

/// Extract domain from the first `/dnsaddr/{domain}` component.
fn extract_domain(addr: &Multiaddr) -> Option<String> {
    addr.iter().find_map(|p| match p {
        Protocol::Dnsaddr(domain) => Some(domain.to_string()),
        _ => None,
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
        assert_eq!(
            extract_domain(&addr),
            Some("mainnet.ethswarm.org".to_string())
        );
    }

    #[test]
    fn extract_domain_returns_none_for_ip() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        assert_eq!(extract_domain(&addr), None);
    }

    #[tokio::test]
    async fn resolve_all_passes_non_dnsaddr_through() {
        let resolver = DnsaddrResolver::from_system_conf().expect("system DNS configuration");
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        let resolution = resolver.resolve_all(std::slice::from_ref(&addr)).await;
        assert_eq!(resolution.addrs, vec![addr]);
        assert_eq!(resolution.min_ttl, None, "no lookup performed, no TTL");
        assert_eq!(
            resolution.invalid_leaves, 0,
            "pass-through inputs are not validated"
        );
        assert!(!resolution.truncated);
    }

    #[tokio::test]
    async fn resolve_recursive_returns_leaf_with_peer_id() {
        let resolver = DnsaddrResolver::from_system_conf().expect("system DNS configuration");
        let addr: Multiaddr = format!("/ip4/127.0.0.1/tcp/1634/p2p/{}", libp2p::PeerId::random())
            .parse()
            .unwrap();
        let mut state = ResolveState::default();
        let resolved = resolver
            .resolve_recursive(&addr, &mut state, 0)
            .await
            .unwrap();
        assert_eq!(resolved, vec![addr]);
        assert_eq!(state.invalid_leaves, 0);
    }

    #[tokio::test]
    async fn resolve_recursive_drops_leaf_without_peer_id() {
        let resolver = DnsaddrResolver::from_system_conf().expect("system DNS configuration");
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        let mut state = ResolveState::default();
        let resolved = resolver
            .resolve_recursive(&addr, &mut state, 0)
            .await
            .unwrap();
        assert!(resolved.is_empty(), "a leaf without /p2p/ is dropped");
        assert_eq!(state.invalid_leaves, 1);
    }

    #[tokio::test]
    async fn resolve_recursive_stops_at_record_cap() {
        let resolver = DnsaddrResolver::from_system_conf().expect("system DNS configuration");
        let addr: Multiaddr = "/dnsaddr/mainnet.ethswarm.org".parse().unwrap();
        let mut state = ResolveState {
            records: MAX_DNSADDR_RECORDS,
            ..ResolveState::default()
        };
        let resolved = resolver
            .resolve_recursive(&addr, &mut state, 0)
            .await
            .unwrap();
        assert!(resolved.is_empty(), "an exhausted budget resolves nothing");
        assert!(state.truncated);
        assert!(
            state.seen.is_empty(),
            "the cap is checked before any lookup"
        );
    }
}
