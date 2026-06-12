//! Browser dnsaddr resolution over DNS-over-HTTPS (DoH).
//!
//! A browser cannot perform raw DNS TXT lookups, so the `/dnsaddr/mainnet.ethswarm.org`
//! indirection the live network relies on cannot be resolved the way the native
//! `vertex-net-dnsaddr` resolver does. This crate restores that indirection on
//! `wasm32` by resolving the dnsaddr tree over DoH: a `fetch` to a DoH endpoint
//! (`application/dns-json`) returns the TXT records, the resolver recurses through
//! the nested `/dnsaddr/` entries, and the browser-dialable secure-WebSocket
//! leaves fall out.
//!
//! # Design
//!
//! - The DoH path is the primary, live source of mainnet bootnodes in the browser.
//! - This crate stays protocol-agnostic: it resolves a dnsaddr tree to dialable
//!   leaves and nothing more. The "fall back to the embedded snapshot when DoH
//!   yields nothing" policy belongs at the bootnode-selection site, which already
//!   knows the network spec, not in this net-layer util. [`resolve_or_fallback`]
//!   is the helper that composes the two given a caller-supplied snapshot.
//! - The recursion driver ([`resolve_dnsaddr`]) is generic over a [`TxtFetcher`],
//!   so its parsing, bounded-depth recursion, de-duplication, and wss-leaf
//!   filtering are unit-tested natively against captured fixtures with no network.
//!   The live browser fetcher ([`DohClient`], wasm-only) supplies the same trait.
//!
//! # Targets
//!
//! The pure logic (parsing, recursion, filtering, [`resolve_or_fallback`]) builds
//! and is tested on every target. The `fetch`-backed [`DohClient`] and the live
//! [`resolve_mainnet_wss_bootnodes`] entrypoint are `wasm32`-only, since `fetch`
//! exists only in the browser. Native clients keep using `vertex-net-dnsaddr`
//! over the system resolver and never link this crate.

pub mod error;
pub mod parse;
pub mod resolver;

#[cfg(target_arch = "wasm32")]
pub mod doh;

pub use error::DohError;
pub use resolver::{DEFAULT_MAX_DEPTH, TxtFetcher, resolve_dnsaddr};

#[cfg(target_arch = "wasm32")]
pub use doh::{CLOUDFLARE_DOH, DohClient, GOOGLE_DOH};

use libp2p::Multiaddr;

/// The mainnet dnsaddr root name (without the `_dnsaddr.` query prefix).
pub const MAINNET_DNSADDR_NAME: &str = "mainnet.ethswarm.org";

/// Resolve `name` over a [`TxtFetcher`], falling back to a snapshot when empty.
///
/// Runs the DoH recursion through `fetcher` and returns its browser-dialable wss
/// leaves. When the live resolution yields nothing (network error, CORS rejection,
/// parse failure, or a genuinely empty tree), the caller-supplied `snapshot` of
/// multiaddr strings is parsed and returned instead, so the caller always receives
/// a dialable set. The chosen path is logged.
///
/// The snapshot is taken as raw strings rather than a hard dependency on a network
/// spec so this net-layer crate stays free of `crates/swarm/` types; the
/// bootnode-selection site passes `vertex_swarm_spec::mainnet_wss_bootnodes()`.
pub async fn resolve_or_fallback<F: TxtFetcher>(
    fetcher: &F,
    name: &str,
    snapshot: &[&str],
) -> Vec<Multiaddr> {
    let resolved = resolve_dnsaddr(fetcher, name, DEFAULT_MAX_DEPTH).await;

    if resolved.is_empty() {
        tracing::warn!(
            %name,
            "DoH dnsaddr resolution yielded no leaves, using embedded snapshot"
        );
        // Rewrite the snapshot's `/ip4/.../tls/sni/<host>/ws` AutoTLS leaves into
        // the `/dns4/<host>/tcp/<port>/tls/ws` form the browser websocket
        // transport dials, matching what the live DoH path returns.
        snapshot
            .iter()
            .filter_map(|s| s.parse().ok())
            .map(|addr| parse::to_browser_dialable_wss(&addr))
            .collect()
    } else {
        tracing::info!(
            %name,
            count = resolved.len(),
            "resolved bootnodes over DoH"
        );
        resolved
    }
}

/// Resolve mainnet bootnodes for a browser client, preferring live DoH.
///
/// Resolves the `mainnet.ethswarm.org` dnsaddr tree over DoH through `client`,
/// falling back to `snapshot` (typically `vertex_swarm_spec::mainnet_wss_bootnodes()`)
/// when the live path yields nothing. See [`resolve_or_fallback`].
#[cfg(target_arch = "wasm32")]
pub async fn resolve_mainnet_wss_bootnodes(
    client: &DohClient,
    snapshot: &[&str],
) -> Vec<Multiaddr> {
    resolve_or_fallback(client, MAINNET_DNSADDR_NAME, snapshot).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use std::future::Future;
    use std::pin::Pin;

    use futures::executor::block_on;

    use super::*;

    struct EmptyFetcher;

    impl TxtFetcher for EmptyFetcher {
        fn fetch_txt(
            &self,
            _name: &str,
        ) -> Pin<Box<dyn Future<Output = Result<String, DohError>> + '_>> {
            Box::pin(async { Err(DohError::EmptyResolution) })
        }
    }

    struct OneLeafFetcher;

    const LEAF: &str = "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg";

    /// `LEAF` after the browser-dialable rewrite (the form both the live and
    /// snapshot paths return).
    const DIALABLE_LEAF: &str = "/dns4/example.libp2p.direct/tcp/1635/tls/ws/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg";

    impl TxtFetcher for OneLeafFetcher {
        fn fetch_txt(
            &self,
            _name: &str,
        ) -> Pin<Box<dyn Future<Output = Result<String, DohError>> + '_>> {
            let body = format!(r#"{{"Answer": [{{"type": 16, "data": "dnsaddr={LEAF}"}}]}}"#);
            Box::pin(async move { Ok(body) })
        }
    }

    #[test]
    fn falls_back_to_snapshot_when_resolution_empty() {
        let snap = [LEAF];
        let leaves = block_on(resolve_or_fallback(
            &EmptyFetcher,
            "mainnet.ethswarm.org",
            &snap,
        ));
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].to_string(), DIALABLE_LEAF);
    }

    #[test]
    fn prefers_live_resolution_over_snapshot() {
        // Snapshot is a different, stale address; live resolution must win.
        let stale = "/ip4/1.2.3.4/tcp/1635/tls/sni/stale.libp2p.direct/ws/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg";
        let snap = [stale];
        let leaves = block_on(resolve_or_fallback(
            &OneLeafFetcher,
            "mainnet.ethswarm.org",
            &snap,
        ));
        assert_eq!(leaves.len(), 1);
        assert_eq!(
            leaves[0].to_string(),
            DIALABLE_LEAF,
            "live leaf must take precedence"
        );
    }
}
