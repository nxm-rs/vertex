//! Cached self-record signing.
//!
//! Signing our own [`SwarmPeer`] record is one ECDSA operation. The advertised
//! address set rarely changes, so signing it on every connection wastes work and
//! produces a different signed record each time (only the timestamp moves). A
//! receiver stores and re-gossips that record, so an advancing timestamp on an
//! otherwise-identical record churns the network for no semantic gain.
//!
//! This module caches the signed record keyed by a fingerprint of the address
//! set. While the set is unchanged the same byte-identical record (same
//! timestamp, same signature) is reused across handshakes. The record is
//! re-signed only when the set changes or when [`SELF_RECORD_REFRESH_INTERVAL`]
//! elapses, so the cached timestamp means "when the advertised set last changed
//! or was last refreshed", not "now at each handshake".

use std::{
    hash::{Hash, Hasher},
    time::Duration,
};

use libp2p::Multiaddr;
use vertex_swarm_peer::{SwarmPeer, Timestamp};

/// How often a stable node re-signs its self record to advance the timestamp.
///
/// A receiver rejects a record whose timestamp falls outside its clock-skew
/// tolerance (`SwarmPeer::parse` returns `TimestampOutsideSkewWindow`; the
/// mainnet window is around six hours). A cached record must therefore be
/// refreshed well inside that window so a long-lived node never ages out of a
/// peer's acceptance range. One hour is comfortably below the skew window and
/// above the gossip-side minimum update interval (300 seconds), so a periodic
/// refresh reads as a normal record update rather than a too-soon replay.
pub(crate) const SELF_RECORD_REFRESH_INTERVAL: Duration = Duration::from_secs(3600);

/// A signed self record cached against the address set that produced it.
#[derive(Clone, Debug)]
pub(crate) struct CachedSelfRecord {
    /// Fingerprint of the ordered address set the record was signed over.
    pub(crate) fingerprint: u64,
    /// When the record was signed (its embedded timestamp).
    pub(crate) signed_at: Timestamp,
    /// The signed record, reused byte-identically while the set is unchanged.
    pub(crate) record: SwarmPeer,
}

/// Fingerprint an already-ordered address set.
///
/// The set is deterministically ordered by the address provider, so it is
/// hashed as-is: two calls with the same ordered set produce the same value,
/// and any add, remove, or reorder changes it.
pub(crate) fn fingerprint(addrs: &[Multiaddr]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    addrs.hash(&mut hasher);
    hasher.finish()
}

/// Decide whether the self record must be re-signed.
///
/// Returns `true` when there is no cached record, when the address set changed
/// (different fingerprint), or when the cached record is at least `refresh` old.
/// Pure and side-effect free so it is cheap to unit test.
pub(crate) fn needs_resign(
    cached: Option<&CachedSelfRecord>,
    fingerprint: u64,
    now: Timestamp,
    refresh: Duration,
) -> bool {
    let Some(cached) = cached else {
        return true;
    };
    if cached.fingerprint != fingerprint {
        return true;
    }
    let elapsed = now.get().saturating_sub(cached.signed_at.get());
    let refresh_secs = i64::try_from(refresh.as_secs()).unwrap_or(i64::MAX);
    elapsed >= refresh_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_at(fingerprint: u64, signed_at: i64) -> CachedSelfRecord {
        // The record body is irrelevant to `needs_resign`; only the fingerprint
        // and timestamp drive the decision. Build a placeholder via a real sign
        // so the type is well formed.
        use alloy_signer_local::LocalSigner;
        use vertex_swarm_peer::SwarmPeer;

        let signer = LocalSigner::random();
        let record = SwarmPeer::sign(
            &signer,
            vec!["/ip4/127.0.0.1/tcp/1634".parse().expect("valid multiaddr")],
            nectar_primitives::SwarmAddress::with_first_byte(0x01),
            1.into(),
            nectar_primitives::Nonce::ZERO,
            Timestamp::from_seconds(signed_at.max(1)),
            None,
        )
        .expect("sign record");
        CachedSelfRecord {
            fingerprint,
            signed_at: Timestamp::from_seconds(signed_at),
            record,
        }
    }

    #[test]
    fn no_cache_needs_resign() {
        assert!(needs_resign(
            None,
            42,
            Timestamp::from_seconds(1_000),
            SELF_RECORD_REFRESH_INTERVAL,
        ));
    }

    #[test]
    fn same_fingerprint_within_interval_keeps_cache() {
        let cached = record_at(42, 1_000);
        // 100s later, well inside the one-hour refresh window.
        assert!(!needs_resign(
            Some(&cached),
            42,
            Timestamp::from_seconds(1_100),
            SELF_RECORD_REFRESH_INTERVAL,
        ));
    }

    #[test]
    fn different_fingerprint_needs_resign() {
        let cached = record_at(42, 1_000);
        assert!(needs_resign(
            Some(&cached),
            43,
            Timestamp::from_seconds(1_100),
            SELF_RECORD_REFRESH_INTERVAL,
        ));
    }

    #[test]
    fn same_fingerprint_past_interval_needs_resign() {
        let cached = record_at(42, 1_000);
        let now = Timestamp::from_seconds(1_000 + SELF_RECORD_REFRESH_INTERVAL.as_secs() as i64);
        assert!(needs_resign(
            Some(&cached),
            42,
            now,
            SELF_RECORD_REFRESH_INTERVAL,
        ));
    }

    #[test]
    fn fingerprint_is_order_sensitive() {
        let a: Multiaddr = "/ip4/8.8.8.8/tcp/1634".parse().expect("valid multiaddr");
        let b: Multiaddr = "/ip4/1.1.1.1/tcp/1634".parse().expect("valid multiaddr");
        assert_ne!(
            fingerprint(&[a.clone(), b.clone()]),
            fingerprint(&[b, a]),
            "reordering the set changes the fingerprint"
        );
    }

    #[test]
    fn fingerprint_is_stable() {
        let a: Multiaddr = "/ip4/8.8.8.8/tcp/1634".parse().expect("valid multiaddr");
        let b: Multiaddr = "/ip4/1.1.1.1/tcp/1634".parse().expect("valid multiaddr");
        assert_eq!(
            fingerprint(&[a.clone(), b.clone()]),
            fingerprint(&[a, b]),
            "the same ordered set produces the same fingerprint"
        );
    }
}
