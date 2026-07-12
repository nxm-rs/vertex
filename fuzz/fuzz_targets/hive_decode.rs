//! Fuzz the hive gossip decode path: batches of signed peer records from an
//! untrusted peer, driven through the exact validate-batch conversion the
//! inbound reader runs (length checks, the two-tier cache, EIP-191 recovery,
//! overlay validation, the /p2p/ requirement).
//!
//! The flat record is fed raw bytes straight through the generated reader;
//! the nested `Peers` batch goes through the real write then read path with
//! adversarial field content, so the domain conversion sees hostile input
//! while the reader stays on writer-produced bytes. The batch is not raw-fed
//! pending the reported nested-length soundness issue in the upstream
//! `quick-protobuf` reader (as in the handshake target). The oracle adds the
//! amplification bound: full-validation successes are proportional to wire
//! bytes (a surviving record carries at least signature, overlay, and nonce),
//! so one frame-capped message cannot amplify into unbounded work.
//!
//! Seeds live in `fuzz/seeds/hive_decode/` and are replayed on stable by
//! `seed_replay_hive_decode` in `crates/swarm/net/hive/src/protocol.rs`.

#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};
use vertex_swarm_net_hive::fuzz::{
    ARBITRARY_NETWORK_ID, MAX_MESSAGE_SIZE, OverlayAddress, ValidationCache, arbitrary_wire_peers,
    encode_peers, proto, validate_peers,
};

/// Bytes a record surviving full validation must carry at minimum: the
/// 65-byte signature, 32-byte overlay, and 32-byte nonce fields.
const MIN_VALID_RECORD_BYTES: usize = 129;

/// Generous batch bound for the generator; the production count bound is the
/// frame cap, which the target asserts against the encoded size.
const MAX_GENERATED_PEERS: usize = 40;

fuzz_target!(|data: &[u8]| {
    let cache = ValidationCache::default();

    // Flat record: raw attacker bytes straight through the generated reader
    // are sound because the record carries no nested message.
    let mut reader = BytesReader::from_bytes(data);
    if let Ok(record) = proto::SwarmPeer::from_reader(&mut reader, data) {
        let local = OverlayAddress::from_slice(&record.overlay)
            .unwrap_or_else(|_| OverlayAddress::new([0u8; 32]));
        let _ = validate_peers(vec![record], ARBITRARY_NETWORK_ID, &local, &cache);
    }

    let mut u = Unstructured::new(data);

    let Ok(batch) = arbitrary_wire_peers(&mut u, MAX_GENERATED_PEERS) else {
        return;
    };
    // Sometimes our own overlay is in the batch, so the self-dial arm fires.
    let local = batch
        .peers
        .first()
        .filter(|_| u.arbitrary().unwrap_or(false))
        .and_then(|p| OverlayAddress::from_slice(&p.overlay).ok())
        .unwrap_or_else(|| OverlayAddress::new([0u8; 32]));

    let mut bytes = Vec::with_capacity(batch.get_size());
    batch
        .write_message(&mut Writer::new(&mut bytes))
        .expect("proto serialization must succeed");
    let mut reader = BytesReader::from_bytes(&bytes);
    let decoded =
        proto::Peers::from_reader(&mut reader, &bytes).expect("serialized frames must deserialize");
    let raw_count = decoded.peers.len();
    // Each repeated record entry costs at least its tag and length bytes, so
    // the frame cap bounds the raw count a conformant frame can carry.
    assert!(raw_count.saturating_mul(2) <= bytes.len());

    let (peers, valid_count, invalid_count) =
        validate_peers(decoded.peers, ARBITRARY_NETWORK_ID, &local, &cache);
    assert_eq!(peers.len(), valid_count);
    assert_eq!(valid_count + invalid_count, raw_count);
    assert!(valid_count.saturating_mul(MIN_VALID_RECORD_BYTES) <= bytes.len());
    if bytes.len() <= MAX_MESSAGE_SIZE {
        // The production frame cap therefore bounds per-message ECDSA wins.
        assert!(valid_count <= MAX_MESSAGE_SIZE / MIN_VALID_RECORD_BYTES);
    }

    // Re-validating the survivors hits the cache tier and reproduces them.
    let reencoded = encode_peers(&peers);
    let (again, revalid, reinvalid) =
        validate_peers(reencoded.peers, ARBITRARY_NETWORK_ID, &local, &cache);
    assert_eq!(revalid, valid_count);
    assert_eq!(reinvalid, 0);
    assert_eq!(again, peers);
});
