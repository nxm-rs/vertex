//! Fuzz-facing surface behind the `arbitrary` feature: the entry-boundary
//! driver shared by the `ffi_entry` fuzz target and the stable seed-replay
//! test. Dev-only; never enabled by the shipped cdylib cone.

use arbitrary::Unstructured;
use nectar_postage::{Batch, STAMP_SIZE, generators::signed_stamped_chunk};
use vertex_swarm_api::StampedChunk;
use vertex_swarm_spec::init_dev;

use crate::api::client::{
    build_identity, build_network, parse_address, parse_stamp, reconstruct_upload, stream_config,
};
use crate::api::logging::parse_filter;
use crate::api::types::{VertexChunkUpload, VertexStreamConfig};
use crate::error::FfiError;

/// Drive one fuzz input through every entry-boundary reconstruction helper.
///
/// The invariant is the crate rule at the cdylib boundary: raw host bytes and
/// strings become strong types or a typed [`FfiError`], never a panic, and a
/// valid-by-construction input is accepted.
pub fn check_entry(data: &[u8]) {
    check_raw_parses(data);

    let mut u = Unstructured::new(data);
    check_identity(&mut u);
    check_raw_upload(&mut u);
    check_valid_upload(&mut u);
    check_stream_config(&mut u);
    check_log_filter(&mut u);
    check_network(u);
}

/// The whole input as a raw address and a raw stamp: the length gates fire
/// first and carry the rejected length.
fn check_raw_parses(data: &[u8]) {
    match parse_address(data) {
        Ok(address) => {
            assert_eq!(data.len(), 32);
            assert_eq!(address.as_bytes(), data);
        }
        Err(FfiError::InvalidAddress { len }) => assert_eq!(len, data.len()),
        Err(e) => panic!("address parse must fail as InvalidAddress: {e}"),
    }

    match parse_stamp(data) {
        Ok(stamp) => {
            assert_eq!(data.len(), STAMP_SIZE);
            // An accepted stamp re-encodes to bytes that parse again.
            assert!(parse_stamp(&stamp.to_bytes()).is_ok());
        }
        Err(FfiError::InvalidStamp { .. }) => {}
        Err(e) => panic!("stamp parse must fail as InvalidStamp: {e}"),
    }
}

/// Identity keys: the 32-byte guard first, then the secp256k1 scalar check.
fn check_identity(u: &mut Unstructured<'_>) {
    let Ok(key) = u.arbitrary::<Option<Vec<u8>>>() else {
        return;
    };
    let spec = init_dev();
    match (build_identity(&spec, key.as_deref()).map(|_| ()), key) {
        (Ok(()), None) => {}
        (Ok(()), Some(key)) => assert_eq!(key.len(), 32),
        (Err(FfiError::InvalidPrivateKey { len }), Some(key)) => {
            assert_eq!(len, key.len());
            assert_ne!(len, 32);
        }
        // A 32-byte value can still be an invalid scalar (zero or over the
        // curve order); that fails after the length guard.
        (Err(FfiError::Build { .. }), Some(key)) => assert_eq!(key.len(), 32),
        (Err(e), _) => panic!("unexpected identity error: {e}"),
    }
}

/// Upload reconstruction from fully adversarial parts: a success pins the
/// chunk to the requested address, anything else is a typed error.
fn check_raw_upload(u: &mut Unstructured<'_>) {
    let Ok((address, payload, stamp)) = u.arbitrary::<(Vec<u8>, Vec<u8>, Vec<u8>)>() else {
        return;
    };
    let upload = VertexChunkUpload {
        address: address.clone(),
        data: payload,
        stamp,
        validate: false,
    };
    if let Ok(stamped) = reconstruct_upload(upload) {
        assert_eq!(stamped.address().as_bytes(), address.as_slice());
    }
}

/// A coherent signed stamped chunk survives decomposition to raw host bytes
/// and reconstruction; a flipped stamp bit stays a parse (never a panic) and
/// a flipped address bit is a typed mismatch.
fn check_valid_upload(u: &mut Unstructured<'_>) {
    let generated: Result<(StampedChunk, Batch), _> = signed_stamped_chunk(u);
    let Ok((stamped, _batch)) = generated else {
        // Not enough entropy left in the input for the valid tier.
        return;
    };
    let address = stamped.address().as_bytes().to_vec();
    let (chunk, stamp) = stamped.into_parts();
    let stamp_bytes = stamp.to_bytes();
    let wire = chunk.into_bytes();

    let rebuilt = reconstruct_upload(VertexChunkUpload {
        address: address.clone(),
        data: wire.to_vec(),
        stamp: stamp_bytes.to_vec(),
        validate: false,
    })
    .unwrap_or_else(|e| panic!("a coherent stamped chunk must reconstruct: {e}"));
    assert_eq!(rebuilt.address().as_bytes(), address.as_slice());

    if let (Ok(idx), Ok(bit)) = (u.choose_index(STAMP_SIZE), u.int_in_range(0..=7u8)) {
        let mut tampered = stamp_bytes;
        if let Some(byte) = tampered.get_mut(idx) {
            *byte ^= 1 << bit;
        }
        let _ = parse_stamp(&tampered);
    }

    let mut wrong = address;
    if let Some(byte) = wrong.first_mut() {
        *byte ^= 0x01;
    }
    let mismatch = reconstruct_upload(VertexChunkUpload {
        address: wrong,
        data: wire.to_vec(),
        stamp: stamp_bytes.to_vec(),
        validate: false,
    });
    assert!(matches!(mismatch, Err(FfiError::ChunkMismatch { .. })));
}

/// The stream-config mapping clamps concurrency to at least one.
fn check_stream_config(u: &mut Unstructured<'_>) {
    let Ok((window_bytes, max_concurrency)) = u.arbitrary() else {
        return;
    };
    let cfg = stream_config(VertexStreamConfig {
        window_bytes,
        max_concurrency,
    });
    assert!(cfg.max_concurrency >= 1);
}

/// The logging filter-directive parse is total: an unparseable directive is a
/// typed Logging error.
fn check_log_filter(u: &mut Unstructured<'_>) {
    let Ok(directive) = u.arbitrary::<String>() else {
        return;
    };
    if let Err(e) = parse_filter(&directive) {
        assert!(matches!(e, FfiError::Logging { .. }));
    }
}

/// Bootnode multiaddr parsing is total over host strings; the tail rides as
/// one lossy string to bias the sweep toward multiaddr-shaped text.
fn check_network(mut u: Unstructured<'_>) {
    if let Ok(bootnodes) = u.arbitrary::<Vec<String>>() {
        let _ = build_network(bootnodes);
    }
    let tail = String::from_utf8_lossy(u.take_rest()).into_owned();
    let _ = build_network(vec![tail]);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Replays the committed fuzz seeds through the exact driver the
    /// `ffi_entry` fuzz target runs, so the stable test gate proves the seeds
    /// stay panic-free without the fuzzer.
    #[test]
    fn seed_replay_ffi_entry() {
        let seed_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/ffi_entry");
        let mut replayed = 0usize;
        for entry in std::fs::read_dir(&seed_dir)
            .unwrap_or_else(|e| panic!("seed dir {} must exist: {e}", seed_dir.display()))
        {
            let data = std::fs::read(entry.unwrap().path()).unwrap();
            check_entry(&data);
            replayed += 1;
        }
        assert!(
            replayed >= 9,
            "expected at least the 9 curated seeds, found {replayed}"
        );
    }

    proptest! {
        // Each case may run identity keygen and chunk hashing; keep it bounded.
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn entry_boundary_never_panics(
            data in proptest::collection::vec(any::<u8>(), 0..2048),
        ) {
            check_entry(&data);
        }
    }
}
