//! The transport-agnostic dnsaddr recursion driver.
//!
//! The driver is generic over a [`TxtFetcher`], so the recursion, de-duplication,
//! bounded depth, and wss-leaf filtering can be exercised natively against a
//! fixture fetcher with no network access. The live browser path supplies a
//! `fetch`-backed fetcher (see the crate root and `doh` module).

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;

use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;
use tracing::{debug, warn};

use crate::error::DohError;
use crate::parse::{
    extract_dnsaddr_values, is_browser_dialable_wss, parse_dns_json, to_browser_dialable_wss,
};

/// Default maximum dnsaddr recursion depth.
///
/// The live tree is root -> region -> city -> concrete leaf, which is three hops.
/// Four bounds that with one hop of slack while still cutting off any
/// CNAME-style loop quickly.
pub const DEFAULT_MAX_DEPTH: u8 = 4;

/// Fetches the TXT records for a `_dnsaddr.<name>` query as a raw DNS-JSON body.
///
/// The resolver only needs the JSON string back; parsing and recursion are owned
/// by the driver. Implementors decide the transport: the browser path issues a
/// DoH `fetch`, and tests return a captured fixture body.
pub trait TxtFetcher {
    /// Fetch the DNS-JSON body for a TXT query of `name`.
    ///
    /// `name` is the bare domain (for example `apac.mainnet.ethswarm.org`); the
    /// implementor is responsible for prefixing `_dnsaddr.` and selecting the
    /// query transport. The returned `String` is the JSON body to be parsed by
    /// [`parse_dns_json`].
    fn fetch_txt(&self, name: &str)
    -> Pin<Box<dyn Future<Output = Result<String, DohError>> + '_>>;
}

/// Resolve `/dnsaddr/<name>` to its browser-dialable wss leaves via a fetcher.
///
/// Recurses through nested `/dnsaddr/` entries up to `max_depth`, de-duplicates
/// visited names and produced leaves, and keeps only the secure-WebSocket leaves
/// a browser can dial (see [`is_browser_dialable_wss`]). A fetch or parse failure
/// at any node is logged and treated as an empty branch rather than aborting the
/// whole resolution, so one unreachable region does not sink the others.
///
/// Returns the collected wss leaves, which may be empty when nothing resolved.
pub async fn resolve_dnsaddr<F: TxtFetcher>(
    fetcher: &F,
    name: &str,
    max_depth: u8,
) -> Vec<Multiaddr> {
    let mut seen_names = HashSet::new();
    let mut leaves = Vec::new();
    let mut seen_leaves = HashSet::new();

    resolve_into(
        fetcher,
        name.to_string(),
        0,
        max_depth,
        &mut seen_names,
        &mut leaves,
        &mut seen_leaves,
    )
    .await;

    leaves
}

/// Recursive worker: boxed because the recursion makes the future self-referential.
fn resolve_into<'a, F: TxtFetcher>(
    fetcher: &'a F,
    name: String,
    depth: u8,
    max_depth: u8,
    seen_names: &'a mut HashSet<String>,
    leaves: &'a mut Vec<Multiaddr>,
    seen_leaves: &'a mut HashSet<String>,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        if depth > max_depth {
            warn!(%name, depth, "dnsaddr recursion depth exceeded, stopping branch");
            return;
        }
        if !seen_names.insert(name.clone()) {
            debug!(%name, "skipping already-seen dnsaddr name");
            return;
        }

        let body = match fetcher.fetch_txt(&name).await {
            Ok(body) => body,
            Err(e) => {
                warn!(%name, error = %e, "dnsaddr DoH fetch failed for branch");
                return;
            }
        };

        let response = match parse_dns_json(&body) {
            Ok(response) => response,
            Err(e) => {
                warn!(%name, error = %e, "dnsaddr DoH response parse failed for branch");
                return;
            }
        };

        for value in extract_dnsaddr_values(&response) {
            let addr: Multiaddr = match value.parse() {
                Ok(addr) => addr,
                Err(e) => {
                    warn!(%value, error = %e, "failed to parse dnsaddr multiaddr");
                    continue;
                }
            };

            if let Some(nested) = nested_dnsaddr_name(&addr) {
                resolve_into(
                    fetcher,
                    nested,
                    depth + 1,
                    max_depth,
                    seen_names,
                    leaves,
                    seen_leaves,
                )
                .await;
            } else if is_browser_dialable_wss(&addr) {
                // Rewrite the network's `/ip4/.../tls/sni/<host>/ws` AutoTLS form
                // into the `/dns4/<host>/tcp/<port>/tls/ws` shape the browser
                // websocket transport can dial, deduplicating on the dialable form.
                let dialable = to_browser_dialable_wss(&addr);
                if seen_leaves.insert(dialable.to_string()) {
                    leaves.push(dialable);
                }
            }
        }
    })
}

/// Extract the domain from the first `/dnsaddr/<domain>` component, if any.
fn nested_dnsaddr_name(addr: &Multiaddr) -> Option<String> {
    addr.iter().find_map(|p| match p {
        Protocol::Dnsaddr(domain) => Some(domain.to_string()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use std::collections::HashMap;

    use futures::executor::block_on;

    use super::*;

    /// A fixture fetcher backed by a name -> JSON-body map. Missing names return
    /// an error, mirroring an NXDOMAIN or network failure.
    struct FixtureFetcher {
        bodies: HashMap<String, String>,
    }

    impl TxtFetcher for FixtureFetcher {
        fn fetch_txt(
            &self,
            name: &str,
        ) -> Pin<Box<dyn Future<Output = Result<String, DohError>> + '_>> {
            let result = self
                .bodies
                .get(name)
                .cloned()
                .ok_or(DohError::EmptyResolution);
            Box::pin(async move { result })
        }
    }

    fn leaf_body(wss: &str, tcp: &str) -> String {
        format!(
            r#"{{"Answer": [
                {{"type": 16, "data": "dnsaddr={wss}"}},
                {{"type": 16, "data": "dnsaddr={tcp}"}}
            ]}}"#
        )
    }

    fn region_body(city: &str) -> String {
        format!(r#"{{"Answer": [{{"type": 16, "data": "dnsaddr=/dnsaddr/{city}"}}]}}"#)
    }

    #[test]
    fn resolves_full_tree_to_wss_leaves() {
        let wss = "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg";
        let tcp = "/ip4/5.78.94.214/tcp/1634/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg";

        let mut bodies = HashMap::new();
        bodies.insert(
            "mainnet.ethswarm.org".to_string(),
            region_body("apac.mainnet.ethswarm.org"),
        );
        bodies.insert(
            "apac.mainnet.ethswarm.org".to_string(),
            region_body("tokyo.mainnet.ethswarm.org"),
        );
        bodies.insert(
            "tokyo.mainnet.ethswarm.org".to_string(),
            leaf_body(wss, tcp),
        );

        let fetcher = FixtureFetcher { bodies };
        let leaves = block_on(resolve_dnsaddr(
            &fetcher,
            "mainnet.ethswarm.org",
            DEFAULT_MAX_DEPTH,
        ));

        assert_eq!(leaves.len(), 1);
        // The resolver rewrites the network's `/ip4/.../tls/sni/<host>/ws` form
        // into the `/dns4/<host>/tcp/<port>/tls/ws` shape the browser dials.
        assert_eq!(
            leaves[0].to_string(),
            "/dns4/example.libp2p.direct/tcp/1635/tls/ws/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg"
        );
    }

    #[test]
    fn deduplicates_repeated_leaves() {
        let wss = "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg";
        let tcp = "/ip4/5.78.94.214/tcp/1634/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg";

        // Two regions both pointing at the same city, which has one wss leaf.
        let mut bodies = HashMap::new();
        bodies.insert(
            "mainnet.ethswarm.org".to_string(),
            r#"{"Answer": [
                {"type": 16, "data": "dnsaddr=/dnsaddr/apac.mainnet.ethswarm.org"},
                {"type": 16, "data": "dnsaddr=/dnsaddr/emea.mainnet.ethswarm.org"}
            ]}"#
            .to_string(),
        );
        bodies.insert(
            "apac.mainnet.ethswarm.org".to_string(),
            region_body("tokyo.mainnet.ethswarm.org"),
        );
        bodies.insert(
            "emea.mainnet.ethswarm.org".to_string(),
            region_body("tokyo.mainnet.ethswarm.org"),
        );
        bodies.insert(
            "tokyo.mainnet.ethswarm.org".to_string(),
            leaf_body(wss, tcp),
        );

        let fetcher = FixtureFetcher { bodies };
        let leaves = block_on(resolve_dnsaddr(
            &fetcher,
            "mainnet.ethswarm.org",
            DEFAULT_MAX_DEPTH,
        ));

        assert_eq!(leaves.len(), 1, "duplicate leaves should collapse");
    }

    #[test]
    fn depth_bound_stops_runaway_recursion() {
        // A self-referential name would loop forever without the seen-set; the
        // depth bound is the secondary guard. Point a name at itself.
        let mut bodies = HashMap::new();
        bodies.insert(
            "loop.ethswarm.org".to_string(),
            region_body("loop.ethswarm.org"),
        );
        let fetcher = FixtureFetcher { bodies };
        let leaves = block_on(resolve_dnsaddr(&fetcher, "loop.ethswarm.org", 2));
        assert!(leaves.is_empty());
    }

    #[test]
    fn missing_branch_does_not_sink_siblings() {
        let wss = "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg";
        let tcp = "/ip4/5.78.94.214/tcp/1634/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg";

        // apac resolves, emea is missing (fetch error). The wss leaf still lands.
        let mut bodies = HashMap::new();
        bodies.insert(
            "mainnet.ethswarm.org".to_string(),
            r#"{"Answer": [
                {"type": 16, "data": "dnsaddr=/dnsaddr/apac.mainnet.ethswarm.org"},
                {"type": 16, "data": "dnsaddr=/dnsaddr/emea.mainnet.ethswarm.org"}
            ]}"#
            .to_string(),
        );
        bodies.insert("apac.mainnet.ethswarm.org".to_string(), leaf_body(wss, tcp));

        let fetcher = FixtureFetcher { bodies };
        let leaves = block_on(resolve_dnsaddr(
            &fetcher,
            "mainnet.ethswarm.org",
            DEFAULT_MAX_DEPTH,
        ));

        assert_eq!(leaves.len(), 1);
    }

    #[test]
    fn empty_response_yields_no_leaves() {
        let mut bodies = HashMap::new();
        bodies.insert(
            "mainnet.ethswarm.org".to_string(),
            r#"{"Status": 0}"#.to_string(),
        );
        let fetcher = FixtureFetcher { bodies };
        let leaves = block_on(resolve_dnsaddr(
            &fetcher,
            "mainnet.ethswarm.org",
            DEFAULT_MAX_DEPTH,
        ));
        assert!(leaves.is_empty());
    }
}
