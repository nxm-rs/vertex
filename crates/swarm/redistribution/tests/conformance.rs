//! Conformance tests against the canonical Swarm reference vectors.
//!
//! These are consensus checks. The vertex sampler and proof of entitlement must
//! be byte-exact, because the same values are verified on chain by
//! `Redistribution.sol`. The fixtures here are authoritative oracles for the
//! Swarm storage-incentives protocol:
//!
//! - The single-CAC transformed-address vector uses the deterministic anchor
//!   `swarm-test-anchor-deterministic!` over a 4 KiB pattern chunk.
//! - `fixtures/inclusion_proofs.json` captures a deterministic regression
//!   scenario (`anchor1 = 100`, `anchor2 = 30`): the 16 sorted sample items
//!   (chunk address, transformed address, full chunk data, type) and the three
//!   inclusion proofs (`proof1`/`proof2`/`proofLast`).
//!
//! The anchor-keyed transformed address itself is a nectar primitive
//! ([`AnyChunk::transformed_address`]); nectar owns its conformance vectors.
//! These tests exercise the *redistribution* layer on top of it: sampling, the
//! reserve-commitment chunk, the witness indices and the inclusion proofs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "conformance test over fixed-shape reference oracle fixtures"
)]

use alloy_primitives::{B256, hex};
use serde::Deserialize;

use vertex_swarm_redistribution::{
    ClaimAnchor, SAMPLE_SIZE, SampleAnchor, SampleItem, make_inclusion_proofs,
    reserve_commitment_content, reserve_sample, witness_indices,
};

use nectar_primitives::{
    Chunk, DefaultAnyChunk, DefaultContentChunk, DefaultHasher, DefaultSingleOwnerChunk,
    SwarmAddress,
};

use vertex_swarm_postage::{BatchId, Stamp, StampIndex};

/// A deterministic synthetic stamp for fixture item `slot`.
///
/// The reference inclusion-proof vectors are stamp independent (they pin the
/// RC/OG/TR BMT geometry, which the stamp never feeds), but a proof of
/// entitlement now requires each witnessed slot to carry the exact stamp it was
/// won with. We therefore pin a distinct, deterministic stamp per slot so the
/// byte-for-byte proof assertions remain unchanged while the proof can also be
/// shown to witness *that precise* stamp (see the exact-stamp test). The batch
/// id is the slot index so different slots carry distinguishable stamps.
fn fixture_stamp(slot: usize) -> Stamp {
    let batch: BatchId = B256::repeat_byte(0xc0 + slot as u8);
    let index = StampIndex::new(slot as u32, slot as u32);
    let sig = alloy_primitives::Signature::test_signature();
    Stamp::with_index(batch, index, 1, sig)
}

// --- single-CAC transformed-address vector -----------------------------------

const ANCHOR_CAC: &[u8] = b"swarm-test-anchor-deterministic!";
const WANT_CHUNK_ADDR: &str = "902406053a7a2f3a17f16097e1d0b4b6a4abeae6b84968f5503ae621f9522e16";
const WANT_TRANSFORMED: &str = "9dee91d1ed794460474ffc942996bd713176731db4581a3c6470fe9862905a60";

/// Reproduce the canonical single-CAC chunk-address and transformed-address
/// vector. The chunk content is 4096 bytes of the repeating pattern `i % 256`.
#[test]
fn cac_transformed_address_matches_reference_vector() {
    let mut content = vec![0u8; 4096];
    for (i, b) in content.iter_mut().enumerate() {
        *b = (i % 256) as u8;
    }

    let chunk = DefaultContentChunk::new(content).unwrap();
    assert_eq!(
        hex::encode(chunk.address().as_slice()),
        WANT_CHUNK_ADDR,
        "chunk address must match the reference vector",
    );

    let any: DefaultAnyChunk = chunk.into();
    let tr = any.transformed_address(ANCHOR_CAC);
    assert_eq!(
        hex::encode(tr.as_slice()),
        WANT_TRANSFORMED,
        "transformed address must match the reference vector",
    );
}

// --- inclusion-proof regression vector ---------------------------------------

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
    let raw = include_str!("fixtures/inclusion_proofs.json");
    serde_json::from_str(raw).expect("oracle JSON must parse")
}

fn h(s: &str) -> B256 {
    B256::from_slice(&hex::decode(s.trim_start_matches("0x")).expect("hex"))
}

/// Decode the sample-time anchor (`anchor1`, a bytes32) from the oracle's hex.
fn sample_anchor(oracle: &Oracle) -> SampleAnchor {
    SampleAnchor::new(h(&oracle.anchor1))
}

/// Decode the claim-time anchor (`anchor2`, a bytes32) from the oracle's hex.
fn claim_anchor(oracle: &Oracle) -> ClaimAnchor {
    ClaimAnchor::new(h(&oracle.anchor2))
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
/// both match the reference.
///
/// Each item is pinned with a deterministic [`fixture_stamp`] so a proof of
/// entitlement can be built (it requires the winning stamp on every witnessed
/// slot). The stamp does not feed the RC/OG/TR BMT proofs, so attaching it
/// leaves every byte-for-byte proof assertion unchanged; it does let the
/// exact-stamp test confirm the proof witnesses *that* stamp.
fn rebuild_items(oracle: &Oracle, sample: SampleAnchor) -> Vec<SampleItem> {
    oracle
        .items
        .iter()
        .enumerate()
        .map(|(slot, it)| {
            let chunk = parse_chunk(it);
            assert_eq!(
                hex::encode(chunk.address().as_slice()),
                it.chunk_address.trim_start_matches("0x"),
                "parsed chunk address must match the reference",
            );

            let item = SampleItem::with_stamp(sample, chunk, fixture_stamp(slot));
            assert_eq!(
                hex::encode(item.transformed_address.as_slice()),
                it.transformed_address.trim_start_matches("0x"),
                "recomputed transformed address must match the reference for {}",
                it.chunk_address,
            );
            item
        })
        .collect()
}

#[test]
fn transformed_addresses_match_reference_for_all_sample_items() {
    let oracle = load_oracle();
    let sample = sample_anchor(&oracle);
    let items = rebuild_items(&oracle, sample);
    assert_eq!(items.len(), SAMPLE_SIZE);
    // rebuild_items asserts every transformed address internally.
}

#[test]
fn reserve_sample_reproduces_reference_sorted_order() {
    let oracle = load_oracle();
    let sample = sample_anchor(&oracle);
    let items = rebuild_items(&oracle, sample);

    // The reference sample is already the sorted 16; feeding it (in any order)
    // to reserve_sample must reproduce the exact same ordering.
    let mut shuffled = items.clone();
    shuffled.reverse();
    let got = reserve_sample(shuffled);

    let want: Vec<_> = items.iter().map(|i| i.transformed_address).collect();
    let got_addrs: Vec<_> = got.iter().map(|i| i.transformed_address).collect();
    assert_eq!(got_addrs, want, "sample order must match the reference");
}

#[test]
fn reserve_commitment_chunk_address_matches_reference() {
    let oracle = load_oracle();
    let sample = sample_anchor(&oracle);
    let items = rebuild_items(&oracle, sample);

    let content = reserve_commitment_content(&items);
    assert_eq!(content.len(), SAMPLE_SIZE * 64);

    let mut hasher = DefaultHasher::new();
    hasher.set_span(content.len() as u64);
    hasher.update(&content);
    let addr = hasher.sum();

    assert_eq!(
        hex::encode(addr.as_slice()),
        oracle.sample_chunk_address.trim_start_matches("0x"),
        "reserve-commitment (sample) chunk address must match the reference",
    );
}

#[test]
fn witness_indices_match_reference() {
    let oracle = load_oracle();
    let claim = claim_anchor(&oracle);
    let idx = witness_indices(claim);
    assert_eq!(idx.challenged[0], oracle.require1);
    assert_eq!(idx.challenged[1], oracle.require2);
    assert_eq!(idx.last, oracle.require3);
    assert_eq!(idx.segment_index, oracle.segment_index);
}

/// The headline conformance check: every proof segment, prove segment and chunk
/// span of the proof of entitlement must equal the reference, byte for byte.
#[test]
fn inclusion_proofs_match_reference_byte_for_byte() {
    let oracle = load_oracle();
    let sample = sample_anchor(&oracle);
    let claim = claim_anchor(&oracle);
    let items = rebuild_items(&oracle, sample);

    let proofs = make_inclusion_proofs(&items, sample, claim).expect("proofs build");

    assert_proof(
        &proofs.0[0],
        &oracle.proof1,
        "proof1 (first challenged slot)",
    );
    assert_proof(
        &proofs.0[1],
        &oracle.proof2,
        "proof2 (second challenged slot)",
    );
    assert_proof(&proofs.0[2], &oracle.proof_last, "proofLast (last slot)");
}

/// Each witness must carry the *exact* stamp the sample slot was won with: the
/// `postage_proof` of witness *k* must be byte-identical to the stamp pinned to
/// the slot that witness opens, never a stamp re-loaded by batch id alone.
#[test]
fn inclusion_proof_witnesses_the_exact_winning_stamp() {
    let oracle = load_oracle();
    let sample = sample_anchor(&oracle);
    let claim = claim_anchor(&oracle);
    let items = rebuild_items(&oracle, sample);

    let idx = witness_indices(claim);
    let proofs = make_inclusion_proofs(&items, sample, claim).expect("proofs build");

    // Submission order is [challenged0, challenged1, last]; each carried stamp
    // must equal the stamp pinned to the very slot it opened.
    for (proof, slot) in [
        (&proofs.0[0], idx.challenged[0]),
        (&proofs.0[1], idx.challenged[1]),
        (&proofs.0[2], idx.last),
    ] {
        let won_with = items[slot].stamp.clone().expect("slot stamp present");
        assert_eq!(
            proof.postage_proof, won_with,
            "witness for slot {slot} must carry that slot's exact winning stamp",
        );
        // The precise identity the consensus rule pins: batch id and the full
        // 8-byte stamp index, not merely the batch.
        assert_eq!(proof.postage_proof.batch(), won_with.batch());
        assert_eq!(
            proof.postage_proof.stamp_index().to_be_bytes(),
            won_with.stamp_index().to_be_bytes(),
            "the full 8-byte stamp index must match",
        );
    }

    // A distinct slot carries a distinct stamp, so a batch-keyed reload (which
    // could not tell two stamps of one batch apart) would not satisfy this.
    assert_ne!(
        proofs.0[0].postage_proof, proofs.0[2].postage_proof,
        "different witnessed slots must carry their own distinct stamps",
    );
}

/// A sample slot that reaches proof time with no pinned stamp is refused, rather
/// than papered over by re-loading a stamp by batch id.
#[test]
fn inclusion_proof_rejects_a_slot_without_a_stamp() {
    use vertex_swarm_redistribution::ProofError;

    let oracle = load_oracle();
    let sample = sample_anchor(&oracle);
    let claim = claim_anchor(&oracle);
    let mut items = rebuild_items(&oracle, sample);

    // Drop the stamp on the last slot (always witnessed).
    let last = items.len() - 1;
    items[last].stamp = None;

    let err = make_inclusion_proofs(&items, sample, claim).expect_err("must reject");
    assert!(
        matches!(err, ProofError::MissingStamp(slot) if slot == last),
        "expected MissingStamp({last}), got {err:?}",
    );
}

/// The same content presented under two different batches is sampled **at most
/// once**, and the committed reserve-commitment bytes are identical regardless
/// of insertion order. Only *which batch's stamp* travels with the surviving
/// slot is order dependent, which is consensus-safe: the chain commits the
/// (order-invariant) `chunk_address || transformed_address` pair and binds the
/// witnessed stamp to the chunk, not to a canonical batch. See the
/// `reserve_sample` function note.
#[test]
fn same_content_multi_batch_ties_to_one_slot_with_stable_commitment() {
    let anchor = SampleAnchor::new(B256::repeat_byte(0x5a));

    // One content chunk, two distinct batches: same chunk address and (under one
    // anchor) the same transformed address, so they tie.
    let chunk: DefaultAnyChunk =
        DefaultContentChunk::new(b"shared payload under two batches".to_vec())
            .expect("cac builds")
            .into();

    let stamp_a = {
        let batch: BatchId = B256::repeat_byte(0xa1);
        Stamp::with_index(
            batch,
            StampIndex::new(1, 1),
            1,
            alloy_primitives::Signature::test_signature(),
        )
    };
    let stamp_b = {
        let batch: BatchId = B256::repeat_byte(0xb2);
        Stamp::with_index(
            batch,
            StampIndex::new(2, 2),
            2,
            alloy_primitives::Signature::test_signature(),
        )
    };

    let item_a = SampleItem::with_stamp(anchor, chunk.clone(), stamp_a.clone());
    let item_b = SampleItem::with_stamp(anchor, chunk, stamp_b.clone());
    assert_eq!(
        item_a.transformed_address, item_b.transformed_address,
        "same content under one anchor must share a transformed address",
    );

    let forward = reserve_sample(vec![item_a.clone(), item_b.clone()]);
    let reverse = reserve_sample(vec![item_b.clone(), item_a.clone()]);

    // At most once: a single slot in either order.
    assert_eq!(forward.len(), 1, "tied content must collapse to one slot");
    assert_eq!(reverse.len(), 1, "tied content must collapse to one slot");

    // The committed bytes (chunk_address || transformed_address) are byte-
    // identical across orders: nothing order dependent reaches the chain.
    assert_eq!(
        reserve_commitment_content(&forward),
        reserve_commitment_content(&reverse),
        "the reserve commitment must be insertion-order invariant",
    );

    // The only order-dependent quantity is which batch's stamp survives; both
    // orders keep the last CAC seen (consensus-safe, documented as order-agnostic
    // against bee). This pins the observed behaviour without asserting it is
    // consensus-binding.
    assert_eq!(
        forward[0].stamp.as_ref().map(Stamp::batch),
        Some(stamp_b.batch()),
        "forward order keeps the last CAC's stamp",
    );
    assert_eq!(
        reverse[0].stamp.as_ref().map(Stamp::batch),
        Some(stamp_a.batch()),
        "reverse order keeps the last CAC's stamp",
    );
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

    // TR (anchor-prefixed) BMT segment proof: proofSegments3. The protocol proves
    // the same segment content as OG, so the proven segment must agree too.
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
    let sample = sample_anchor(&oracle);
    let claim = claim_anchor(&oracle);
    let items = rebuild_items(&oracle, sample);

    let content = reserve_commitment_content(&items);
    let mut rc = DefaultHasher::new();
    rc.set_span(content.len() as u64);
    rc.update(&content);
    let rc_root = rc.sum();

    let proofs = make_inclusion_proofs(&items, sample, claim).expect("proofs build");
    let idx = witness_indices(claim);

    for (p, require) in [
        (&proofs.0[0], idx.challenged[0]),
        (&proofs.0[1], idx.challenged[1]),
        (&proofs.0[2], idx.last),
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
                .verify(prefixed_root(&items[require].chunk, sample.as_bytes()).as_slice())
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
