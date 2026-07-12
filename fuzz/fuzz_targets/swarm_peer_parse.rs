//! Fuzz the peer-record parse boundary: the hand-rolled multiaddr block
//! decoder on raw bytes, `SwarmPeer::parse` (EIP-191 signature recovery,
//! overlay validation, chequebook and timestamp checks) on adversarial wire
//! fields, and the receiver-side gossip timestamp policy.
//!
//! Half the wire records are really signed with at most one tampered field,
//! so recovery success and every rejection arm stay covered. Any returned
//! `Err` is success; the oracle is "no panic, no OOM, no hang".
//!
//! Seeds live in `fuzz/seeds/swarm_peer_parse/` (multiaddr blocks, the raw
//! arm's input) and are replayed on stable by `seed_replay_swarm_peer_parse`
//! in `crates/swarm/peers/peer/src/serde_multiaddr.rs`.

#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;
use vertex_swarm_peer::fuzz::{
    ARBITRARY_NETWORK_ID, Duration, MAX_CLOCK_SKEW, MAX_MULTIADDRS_PER_PEER, NetworkId, SwarmPeer,
    Timestamp, arbitrary_wire_record, check_timestamp, deserialize_multiaddrs,
    reencodes_ambiguously, serialize_multiaddrs,
};

fuzz_target!(|data: &[u8]| {
    // Hand-rolled multiaddr block decoder straight on raw bytes: the count
    // cap holds and the canonical re-encode round-trips.
    if let Ok(addrs) = deserialize_multiaddrs(data) {
        assert!(addrs.len() <= MAX_MULTIADDRS_PER_PEER);
        // A lone multiaddr whose bytes open with the list prefix cannot be
        // told apart from a list on decode, so the single-addr encoding only
        // re-decodes for every other shape.
        if !reencodes_ambiguously(&addrs) {
            let bytes = serialize_multiaddrs(&addrs);
            let again = deserialize_multiaddrs(&bytes).expect("re-encoded block decodes");
            assert_eq!(again, addrs);
        }
    }

    let mut u = Unstructured::new(data);

    // Wire-record parse under the signing id, a mismatched id, and an
    // adversarial clock-skew window.
    if let Ok(record) = arbitrary_wire_record(&mut u) {
        if let Ok(peer) = SwarmPeer::parse(record.as_wire(), ARBITRARY_NETWORK_ID, None) {
            assert!(!peer.multiaddrs().is_empty());
            assert!(peer.timestamp().get() > 0);
        }
        let _ = SwarmPeer::parse(record.as_wire(), NetworkId::new(0), None);

        if let (Ok(now), Ok(tolerance)) = (u.arbitrary::<i64>(), u.arbitrary::<u32>()) {
            let skew = Some((
                Timestamp::from_seconds(now),
                Duration::from_secs(u64::from(tolerance)),
            ));
            let _ = SwarmPeer::parse(record.as_wire(), ARBITRARY_NETWORK_ID, skew);
        }
    }

    // Receiver-side gossip timestamp policy over the full i64 range: an
    // accepted candidate is strictly positive and within the future bound.
    if let (Ok(candidate), Ok(existing), Ok(now)) = (
        u.arbitrary::<i64>(),
        u.arbitrary::<Option<i64>>(),
        u.arbitrary::<i64>(),
    ) {
        let accepted = check_timestamp(
            Timestamp::from_seconds(candidate),
            existing.map(Timestamp::from_seconds),
            Timestamp::from_seconds(now),
        );
        if accepted.is_ok() {
            assert!(candidate > 0);
            let max_skew = i64::try_from(MAX_CLOCK_SKEW.as_secs()).unwrap_or(i64::MAX);
            assert!(candidate <= now.saturating_add(max_skew));
        }
    }
});
