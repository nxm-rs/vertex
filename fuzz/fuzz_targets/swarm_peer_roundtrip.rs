//! Structured round-trip fuzz of the peer record.
//!
//! Inputs come from the shared generator layer: the `arbitrary` impl of
//! `SwarmPeer` mints a really signed record under the generator network id.
//! The oracle is stronger than "no panic": the wire fields must parse back to
//! an equal record, signature recovery included, and the clock-skew window
//! must hold exactly at its inclusive boundaries.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vertex_swarm_peer::fuzz::{
    ARBITRARY_NETWORK_ID, Duration, SwarmPeer, SwarmPeerError, Timestamp, WireRecord,
};

const TOLERANCE_SECS: i64 = 60;

fuzz_target!(|peer: SwarmPeer| {
    let record = WireRecord::from_peer(&peer);
    let parsed = SwarmPeer::parse(record.as_wire(), ARBITRARY_NETWORK_ID, None)
        .expect("a validly signed record parses");
    assert_eq!(parsed, peer);

    // Inclusive skew boundaries around the record's own timestamp (the
    // generator caps it well below i64::MAX, so the shifts cannot overflow).
    let tolerance = Duration::from_secs(TOLERANCE_SECS.unsigned_abs());
    let at_boundary = Timestamp::from_seconds(peer.timestamp().get() + TOLERANCE_SECS);
    SwarmPeer::parse(
        record.as_wire(),
        ARBITRARY_NETWORK_ID,
        Some((at_boundary, tolerance)),
    )
    .expect("the inclusive boundary is accepted");

    let past_boundary = Timestamp::from_seconds(peer.timestamp().get() + TOLERANCE_SECS + 1);
    assert!(matches!(
        SwarmPeer::parse(
            record.as_wire(),
            ARBITRARY_NETWORK_ID,
            Some((past_boundary, tolerance)),
        ),
        Err(SwarmPeerError::TimestampOutsideSkewWindow)
    ));
});
