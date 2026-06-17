//! Cross-implementation parity tests against bee's published vectors.
//!
//! These are consensus checks. The vertex sampler and proof of entitlement must
//! reproduce Swarm's reference implementation (bee) byte for byte, because the
//! same values are verified on chain by `Redistribution.sol`. The fixtures here
//! are authoritative oracles extracted directly from bee's Go test suite:
//!
//! - The single-CAC transformed-address vector is bee's `TestSampleVectorCAC`
//!   (`pkg/storer/sample_test.go`), anchor `swarm-test-anchor-deterministic!`.
//! - `fixtures/bee_inclusion_proofs.json` is generated from bee's deterministic
//!   `TestMakeInclusionProofsRegression` scenario (`pkg/storageincentives`,
//!   anchor1 = 100, anchor2 = 30): the 16 sorted sample items (chunk address,
//!   transformed address, full chunk data, type) and the three inclusion proofs
//!   (`proof1`/`proof2`/`proofLast`), captured via an oracle extractor run
//!   against the live bee tree.
//!
//! The anchor-keyed transformed address itself is a nectar primitive
//! ([`AnyChunk::transformed_address`]); nectar owns its parity oracle. These
//! tests exercise the *redistribution* layer on top of it: sampling, the
//! reserve-commitment chunk, the witness indices and the inclusion proofs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "parity test over fixed-shape bee oracle fixtures"
)]

use alloy_primitives::{B256, hex};
use serde::Deserialize;

use vertex_swarm_redistribution::{
    SAMPLE_SIZE, SampleItem, make_inclusion_proofs, reserve_commitment_content, reserve_sample,
    witness_indices,
};

use nectar_primitives::{
    Chunk, DefaultAnyChunk, DefaultContentChunk, DefaultHasher, DefaultSingleOwnerChunk,
    SwarmAddress,
};

// --- bee TestSampleVectorCAC -------------------------------------------------

const ANCHOR_CAC: &[u8] = b"swarm-test-anchor-deterministic!";
const WANT_CHUNK_ADDR: &str = "902406053a7a2f3a17f16097e1d0b4b6a4abeae6b84968f5503ae621f9522e16";
const WANT_TRANSFORMED: &str = "9dee91d1ed794460474ffc942996bd713176731db4581a3c6470fe9862905a60";

/// Reproduce bee's published single-CAC chunk-address and transformed-address
/// vector. The chunk content is 4096 bytes of the repeating pattern `i % 256`.
#[test]
fn cac_transformed_address_matches_bee_vector() {
    let mut content = vec![0u8; 4096];
    for (i, b) in content.iter_mut().enumerate() {
        *b = (i % 256) as u8;
    }

    let chunk = DefaultContentChunk::new(content).unwrap();
    assert_eq!(
        hex::encode(chunk.address().as_slice()),
        WANT_CHUNK_ADDR,
        "chunk address must match bee's published vector",
    );

    let any: DefaultAnyChunk = chunk.into();
    let tr = any.transformed_address(ANCHOR_CAC);
    assert_eq!(
        hex::encode(tr.as_slice()),
        WANT_TRANSFORMED,
        "transformed address must match bee's published vector",
    );
}

// --- bee TestMakeInclusionProofsRegression -----------------------------------

#[derive(Deserialize)]
struct OracleItem {
    #[serde(rename = "chunkType")]
    chunk_type: String,
    #[serde(rename = "chunkAddress")]
    chunk_address: String,
    #[serde(rename = "transformedAddress")]
    transformed_address: String,
    #[serde(rename = "chunkData")]
    chunk_data: String,
}

#[derive(Deserialize)]
struct OracleProof {
    #[serde(rename = "proofSegments")]
    proof_segments: Vec<String>,
    #[serde(rename = "proveSegment")]
    prove_segment: String,
    #[serde(rename = "proofSegments2")]
    proof_segments2: Vec<String>,
    #[serde(rename = "proveSegment2")]
    prove_segment2: String,
    #[serde(rename = "chunkSpan")]
    chunk_span: u64,
    #[serde(rename = "proofSegments3")]
    proof_segments3: Vec<String>,
}

#[derive(Deserialize)]
struct Oracle {
    anchor1: String,
    anchor2: String,
    require1: usize,
    require2: usize,
    require3: usize,
    #[serde(rename = "segmentIndex")]
    segment_index: usize,
    #[serde(rename = "sampleChunkAddress")]
    sample_chunk_address: String,
    items: Vec<OracleItem>,
    proof1: OracleProof,
    proof2: OracleProof,
    #[serde(rename = "proofLast")]
    proof_last: OracleProof,
}

fn load_oracle() -> Oracle {
    let raw = include_str!("fixtures/bee_inclusion_proofs.json");
    serde_json::from_str(raw).expect("oracle JSON must parse")
}

fn h(s: &str) -> B256 {
    B256::from_slice(&hex::decode(s.trim_start_matches("0x")).expect("hex"))
}

/// Parse one oracle item's raw chunk wire bytes into the typed [`AnyChunk`] the
/// sampler operates on. A `CAC` is a `span || payload` content body; a `SOC` is
/// `id || signature || span || payload`. The chunk's `TryFrom` enforces the
/// minimum sizes, so malformed bytes become parse errors, not panics.
fn parse_chunk(it: &OracleItem) -> DefaultAnyChunk {
    let bytes = hex::decode(&it.chunk_data).expect("chunk data hex");
    if it.chunk_type == "SOC" {
        DefaultSingleOwnerChunk::try_from(bytes.as_slice())
            .expect("SOC chunk parses")
            .into()
    } else {
        DefaultContentChunk::try_from(bytes.as_slice())
            .expect("CAC chunk parses")
            .into()
    }
}

/// Rebuild every sample item from the oracle (typed chunk + transformed address),
/// asserting the parsed chunk address and the recomputed transformed address
/// both match bee.
fn rebuild_items(oracle: &Oracle, anchor1: &[u8]) -> Vec<SampleItem> {
    oracle
        .items
        .iter()
        .map(|it| {
            let chunk = parse_chunk(it);
            assert_eq!(
                hex::encode(chunk.address().as_slice()),
                it.chunk_address.trim_start_matches("0x"),
                "parsed chunk address must match bee",
            );

            let item = SampleItem::new(anchor1, chunk);
            assert_eq!(
                hex::encode(item.transformed_address.as_slice()),
                it.transformed_address.trim_start_matches("0x"),
                "recomputed transformed address must match bee for {}",
                it.chunk_address,
            );
            item
        })
        .collect()
}

#[test]
fn transformed_addresses_match_bee_for_all_sample_items() {
    let oracle = load_oracle();
    let anchor1 = hex::decode(&oracle.anchor1).expect("anchor1 hex");
    let items = rebuild_items(&oracle, &anchor1);
    assert_eq!(items.len(), SAMPLE_SIZE);
    // rebuild_items asserts every transformed address internally.
}

#[test]
fn reserve_sample_reproduces_bee_sorted_order() {
    let oracle = load_oracle();
    let anchor1 = hex::decode(&oracle.anchor1).expect("anchor1 hex");
    let items = rebuild_items(&oracle, &anchor1);

    // bee's sample is already the sorted 16; feeding it (in any order) to
    // reserve_sample must reproduce the exact same ordering.
    let mut shuffled = items.clone();
    shuffled.reverse();
    let got = reserve_sample(shuffled);

    let want: Vec<_> = items.iter().map(|i| i.transformed_address).collect();
    let got_addrs: Vec<_> = got.iter().map(|i| i.transformed_address).collect();
    assert_eq!(got_addrs, want, "sample order must match bee");
}

#[test]
fn reserve_commitment_chunk_address_matches_bee() {
    let oracle = load_oracle();
    let anchor1 = hex::decode(&oracle.anchor1).expect("anchor1 hex");
    let items = rebuild_items(&oracle, &anchor1);

    let content = reserve_commitment_content(&items);
    assert_eq!(content.len(), SAMPLE_SIZE * 64);

    let mut hasher = DefaultHasher::new();
    hasher.set_span(content.len() as u64);
    hasher.update(&content);
    let addr = hasher.sum();

    assert_eq!(
        hex::encode(addr.as_slice()),
        oracle.sample_chunk_address.trim_start_matches("0x"),
        "reserve-commitment (sample) chunk address must match bee",
    );
}

#[test]
fn witness_indices_match_bee() {
    let oracle = load_oracle();
    let anchor2 = hex::decode(&oracle.anchor2).expect("anchor2 hex");
    let idx = witness_indices(&anchor2);
    assert_eq!(idx.require1, oracle.require1);
    assert_eq!(idx.require2, oracle.require2);
    assert_eq!(idx.require3, oracle.require3);
    assert_eq!(idx.segment_index, oracle.segment_index);
}

/// The headline parity check: every proof segment, prove segment and chunk span
/// of the proof of entitlement must equal bee's, byte for byte.
#[test]
fn inclusion_proofs_match_bee_byte_for_byte() {
    let oracle = load_oracle();
    let anchor1 = hex::decode(&oracle.anchor1).expect("anchor1 hex");
    let anchor2 = hex::decode(&oracle.anchor2).expect("anchor2 hex");
    let items = rebuild_items(&oracle, &anchor1);

    let proofs = make_inclusion_proofs(&items, &anchor1, &anchor2).expect("proofs build");

    assert_proof(&proofs.0[0], &oracle.proof1, "proof1 (require1)");
    assert_proof(&proofs.0[1], &oracle.proof2, "proof2 (require2)");
    assert_proof(&proofs.0[2], &oracle.proof_last, "proofLast (require3)");
}

fn assert_proof(
    got: &vertex_swarm_redistribution::ChunkInclusionProof,
    want: &OracleProof,
    label: &str,
) {
    // RC chunk inclusion proof: proofSegments / proveSegment.
    assert_eq!(
        got.rc_proof.segment,
        h(&want.prove_segment),
        "{label}: RC prove segment",
    );
    assert_segments(
        &got.rc_proof.proof_segments,
        &want.proof_segments,
        label,
        "RC",
    );

    // OG (plain) BMT segment proof: proofSegments2 / proveSegment2 / chunkSpan.
    assert_eq!(
        got.og_proof.segment,
        h(&want.prove_segment2),
        "{label}: OG prove segment",
    );
    assert_segments(
        &got.og_proof.proof_segments,
        &want.proof_segments2,
        label,
        "OG",
    );
    assert_eq!(got.chunk_span, want.chunk_span, "{label}: chunk span");

    // TR (anchor-prefixed) BMT segment proof: proofSegments3. bee proves the
    // same segment content as OG, so the proven segment must agree too.
    assert_eq!(
        got.tr_proof.segment,
        h(&want.prove_segment2),
        "{label}: TR prove segment (same content as OG)",
    );
    assert_segments(
        &got.tr_proof.proof_segments,
        &want.proof_segments3,
        label,
        "TR",
    );
}

fn assert_segments(got: &[B256], want: &[String], label: &str, which: &str) {
    assert_eq!(
        got.len(),
        want.len(),
        "{label}: {which} proof segment count",
    );
    for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        assert_eq!(*g, h(w), "{label}: {which} proof segment {i}");
    }
}

/// Each witnessed proof must self-verify against the relevant BMT root, a
/// secondary guard on top of the byte-for-byte fixture comparison.
#[test]
fn witness_proofs_self_verify() {
    let oracle = load_oracle();
    let anchor1 = hex::decode(&oracle.anchor1).expect("anchor1 hex");
    let anchor2 = hex::decode(&oracle.anchor2).expect("anchor2 hex");
    let items = rebuild_items(&oracle, &anchor1);

    let content = reserve_commitment_content(&items);
    let mut rc = DefaultHasher::new();
    rc.set_span(content.len() as u64);
    rc.update(&content);
    let rc_root = rc.sum();

    let proofs = make_inclusion_proofs(&items, &anchor1, &anchor2).expect("proofs build");
    let idx = witness_indices(&anchor2);

    for (p, require) in [
        (&proofs.0[0], idx.require1),
        (&proofs.0[1], idx.require2),
        (&proofs.0[2], idx.require3),
    ] {
        assert!(
            p.rc_proof.verify(rc_root.as_slice()).expect("rc verify"),
            "RC proof must verify against the reserve-commitment root",
        );

        // OG proof verifies against the witnessed body's plain BMT root.
        assert!(
            p.og_proof
                .verify(plain_root(&items[require].chunk).as_slice())
                .expect("og verify"),
            "OG proof must verify against the chunk's plain BMT root",
        );

        // TR proof verifies against the inner body's anchor-prefixed BMT root.
        // For a CAC that root *is* the transformed address; for a SOC the
        // transformed address is keccak(soc_addr || this root), so we verify
        // against the prefixed root directly.
        assert!(
            p.tr_proof
                .verify(prefixed_root(&items[require].chunk, &anchor1).as_slice())
                .expect("tr verify"),
            "TR proof must verify against the anchor-prefixed BMT root",
        );
    }
}

/// The plain BMT root of the witnessed chunk body. For a CAC this is the chunk
/// address; for a SOC it is the wrapped CAC's address. The typed `span`/`data`
/// accessors already expose the inner body, so there is no header slicing.
fn plain_root(chunk: &DefaultAnyChunk) -> SwarmAddress {
    let mut hasher = DefaultHasher::new();
    hasher.set_span(chunk.span());
    hasher.update(chunk.data());
    SwarmAddress::from(hasher.sum())
}

/// The anchor-prefixed BMT root of the witnessed chunk body (the TR proof's
/// root). For a CAC this equals the transformed address; for a SOC it is the
/// inner component of the transformed address.
fn prefixed_root(chunk: &DefaultAnyChunk, anchor: &[u8]) -> SwarmAddress {
    let mut hasher = DefaultHasher::with_prefix(anchor);
    hasher.set_span(chunk.span());
    hasher.update(chunk.data());
    SwarmAddress::from(hasher.sum())
}
