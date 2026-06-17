//! Redistribution-game primitives.
//!
//! Pure compute helpers underpinning the storage-incentives redistribution
//! game: a deterministic reserve sample over a neighbourhood and a proof of
//! entitlement over that sample. Everything here is a pure function of its
//! inputs, with no I/O, no storage, no node, and no async machinery, so it
//! produces identical results on every participating node given identical
//! inputs.
//!
//! # Consensus parity
//!
//! This is consensus code. The transformed addresses, the selected sample and
//! the inclusion proofs must match Swarm's reference implementation (bee) byte
//! for byte, because the same values are checked on chain by the
//! `Redistribution.sol` storage-incentives contract. Any divergence makes
//! vertex lose (or be slashed in) the redistribution round. The Go reference
//! lives in bee's `pkg/storer/sample.go` (sampling) and
//! `pkg/storageincentives/proof.go` (proof of entitlement); the Rust here
//! mirrors them deliberately and is validated against bee's published vectors
//! in `tests/`.
//!
//! The building blocks are:
//!
//! - [`canonical_neighbourhood`] filters chunk addresses to those a node is
//!   responsible for at a given committed depth.
//! - [`transformed_address`] computes a chunk's anchor-keyed transformed
//!   address (the value the sample is ordered by).
//! - [`SampleItem`] / [`reserve_sample`] select the [`SAMPLE_SIZE`] chunks with
//!   the lexicographically smallest transformed addresses.
//! - [`make_inclusion_proofs`] / [`ChunkInclusionProof`] build the proof of
//!   entitlement (the witness data submitted to the contract).

use alloy_primitives::{B256, Keccak256};

use nectar_primitives::bmt::Prover;
use nectar_primitives::error::PrimitivesError;
use nectar_primitives::{ChunkAddress, DefaultHasher, Proof, SwarmAddress};

/// Errors arising while building a proof of entitlement.
#[derive(Debug, thiserror::Error)]
pub enum ProofError {
    /// The sample did not contain exactly [`SAMPLE_SIZE`] items.
    #[error("reserve sample must have {SAMPLE_SIZE} items, got {0}")]
    SampleSize(usize),
    /// The sample-time anchor (`anchor1`) was empty.
    #[error("anchor1 is not set")]
    MissingAnchor1,
    /// The claim-time anchor (`anchor2`) was empty.
    #[error("anchor2 is not set")]
    MissingAnchor2,
    /// A BMT proof could not be generated.
    #[error(transparent)]
    Bmt(#[from] PrimitivesError),
}

/// Number of chunks retained in a reserve sample (bee `SampleSize`).
pub const SAMPLE_SIZE: usize = 16;

/// BMT span header size in bytes (a little-endian `u64`).
const SPAN_SIZE: usize = 8;

/// SOC header size preceding the wrapped CAC: 32-byte id + 65-byte signature.
///
/// This is bee's `swarm.HashSize + swarm.SocSignatureSize` and is the byte
/// offset at which a single-owner chunk's inner content-addressed chunk (its
/// span and payload) begins inside the full chunk data.
const SOC_SPAN_OFFSET: usize = 32 + 65;

/// The deterministic neighbourhood for `anchor` at the given committed `depth`.
///
/// Returns the subset of `addrs` a node is responsible for, i.e. those whose
/// proximity order to `anchor` is at least `depth` (bee's
/// `swarm.Proximity(addr, anchor) >= committedDepth` membership test in
/// `ReserveSample`). A `depth` of `0` admits every address.
///
/// Unlike the previous vertex-internal implementation this does **not** impose
/// an XOR-distance ordering. bee never orders the neighbourhood by distance: it
/// streams the depth-filtered chunks and orders the *sample* by transformed
/// address (see [`reserve_sample`]). Imposing an extra distance sort here would
/// be dead work at best and a parity hazard at worst, so the membership filter
/// is all this function does. Callers that need a sample must feed the result
/// (or any iteration order) into [`reserve_sample`], whose output order is
/// fixed by the transformed addresses and therefore independent of input order.
///
/// # Examples
///
/// ```
/// use vertex_swarm_redistribution::canonical_neighbourhood;
/// use nectar_primitives::SwarmAddress;
/// use alloy_primitives::B256;
///
/// let anchor = SwarmAddress::zero();
/// let near = SwarmAddress::from(B256::ZERO);
/// let far = SwarmAddress::from(B256::repeat_byte(0xff));
/// let hood = canonical_neighbourhood(&anchor, 1, [near, far]);
/// assert_eq!(hood, vec![near]);
/// ```
#[must_use]
pub fn canonical_neighbourhood(
    anchor: &SwarmAddress,
    depth: u8,
    addrs: impl IntoIterator<Item = ChunkAddress>,
) -> Vec<ChunkAddress> {
    addrs
        .into_iter()
        .filter(|addr| u8::from(anchor.proximity(addr)) >= depth)
        .collect()
}

/// Compute a chunk's anchor-keyed transformed address.
///
/// This is bee's `transformedAddress` (`pkg/storer/sample.go`): the value the
/// reserve sample is ordered by. `anchor` is the round's sampling salt
/// (`anchor1`), applied as a per-node BMT prefix via nectar's
/// [`DefaultHasher::with_prefix`].
///
/// - **CAC** (`is_soc == false`): the transformed address is the prefixed BMT
///   of the chunk's own span and payload. `chunk_data` is `span(8) || payload`;
///   the span is taken from the first 8 bytes and the payload (everything after
///   it) is hashed under the anchor prefix.
/// - **SOC** (`is_soc == true`): the wrapped content-addressed chunk begins at
///   [`SOC_SPAN_OFFSET`] (after the 32-byte id and 65-byte signature). The
///   transformed address is the **plain** `keccak256(soc_address ||
///   prefixed_bmt(anchor, inner_cac))`. The outer keccak is *not* prefixed —
///   only the inner BMT carries the anchor, matching bee's
///   `transformedAddressSOC` which uses `swarm.NewHasher()` (a plain keccak) for
///   the outer hash.
///
/// # Panics
///
/// Panics if `chunk_data` is shorter than the span header it must contain
/// (8 bytes for a CAC, [`SOC_SPAN_OFFSET`] + 8 for a SOC). Reserve chunks are
/// always well formed, so this only fires on programmer error.
#[must_use]
#[allow(
    clippy::indexing_slicing,
    reason = "reserve chunk data is fixed-layout consensus input; the panic contract is documented above"
)]
pub fn transformed_address(
    anchor: &[u8],
    soc_address: &ChunkAddress,
    chunk_data: &[u8],
    is_soc: bool,
) -> ChunkAddress {
    let span_offset = if is_soc { SOC_SPAN_OFFSET } else { 0 };
    let inner = transformed_address_cac(anchor, &chunk_data[span_offset..]);

    if !is_soc {
        return SwarmAddress::from(inner);
    }

    // SOC: plain (unprefixed) keccak256(soc_address || inner_transformed).
    let mut hasher = Keccak256::new();
    hasher.update(soc_address.as_slice());
    hasher.update(inner.as_slice());
    SwarmAddress::from(B256::from_slice(hasher.finalize().as_slice()))
}

/// Prefixed BMT of a content-addressed chunk body (`span(8) || payload`).
#[allow(
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "CAC body always carries an 8-byte span header; the slice bounds are fixed by the chunk format"
)]
fn transformed_address_cac(anchor: &[u8], cac_data: &[u8]) -> B256 {
    let span = u64::from_le_bytes(
        cac_data[..SPAN_SIZE]
            .try_into()
            .expect("CAC data must contain an 8-byte span header"),
    );
    let mut hasher = DefaultHasher::with_prefix(anchor);
    hasher.set_span(span);
    hasher.update(&cac_data[SPAN_SIZE..]);
    hasher.sum()
}

/// A single entry in a reserve sample.
///
/// Mirrors bee's `storer.SampleItem`. It carries the full chunk data so that
/// the proof of entitlement can re-derive both the original (OG) and
/// transformed (TR) BMT proofs, plus the stamp witness submitted on chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleItem {
    /// The anchor-keyed transformed address the sample is ordered by.
    pub transformed_address: ChunkAddress,
    /// The chunk's own (content or single-owner) address.
    pub chunk_address: ChunkAddress,
    /// The full chunk data: `span||payload` for a CAC, `id||sig||span||payload`
    /// for a SOC.
    pub chunk_data: Vec<u8>,
    /// Whether this chunk is a single-owner chunk.
    pub is_soc: bool,
    /// The postage stamp witness for the chunk.
    pub stamp: Stamp,
}

/// A postage stamp witness, as carried into the proof of entitlement.
///
/// The fields map onto bee's `PostageProof` (`batch_id`, `index`, `timestamp`,
/// `sig`). Index and timestamp are stored big-endian exactly as they appear on
/// the wire so the on-chain `uint64` decode matches.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Stamp {
    /// The postage batch identifier.
    pub batch_id: B256,
    /// The 8-byte big-endian stamp index.
    pub index: [u8; 8],
    /// The 8-byte big-endian stamp timestamp.
    pub timestamp: [u8; 8],
    /// The stamp signature (65 bytes in practice).
    pub sig: Vec<u8>,
}

/// Select the reserve sample from `candidates`.
///
/// Keeps the [`SAMPLE_SIZE`] chunks with the lexicographically smallest
/// transformed addresses, returned in ascending transformed-address order. This
/// is bee's `ReserveSample` selection (`insert`/`le` in `pkg/storer/sample.go`):
/// a sorted insertion that drops the largest element once the sample is full.
///
/// On a transformed-address tie bee keeps the **content-addressed** chunk (the
/// equal-address branch of `insert` replaces the incumbent only when the new
/// item is *not* a valid SOC), so that the on-chain ordering check cannot be
/// gamed by a single-owner chunk colliding with a CAC. We reproduce that exact
/// tie-break.
///
/// `candidates` may be supplied in any order; the output order depends only on
/// the transformed addresses.
#[must_use]
pub fn reserve_sample(candidates: impl IntoIterator<Item = SampleItem>) -> Vec<SampleItem> {
    let mut sample: Vec<SampleItem> = Vec::with_capacity(SAMPLE_SIZE + 1);

    for item in candidates {
        insert_sample_item(&mut sample, item);
    }

    sample
}

/// Insert `item` into the running sorted sample, bee `insert` semantics.
#[allow(
    clippy::indexing_slicing,
    reason = "indices range over the sample's own length, established by the enclosing loop bound"
)]
fn insert_sample_item(sample: &mut Vec<SampleItem>, item: SampleItem) {
    for i in 0..sample.len() {
        match item
            .transformed_address
            .as_slice()
            .cmp(sample[i].transformed_address.as_slice())
        {
            core::cmp::Ordering::Less => {
                sample.insert(i, item);
                // bee trims to SampleSize after a sorted insertion (its slice
                // re-append can transiently overshoot by one).
                if sample.len() > SAMPLE_SIZE {
                    sample.truncate(SAMPLE_SIZE);
                }
                return;
            }
            core::cmp::Ordering::Equal => {
                // Tie on the transformed address: bee replaces the incumbent
                // only when the new chunk is a CAC (not a valid SOC), so a CAC
                // always wins the slot. Either way no new slot is consumed.
                if !item.is_soc {
                    sample[i] = item;
                }
                return;
            }
            core::cmp::Ordering::Greater => {}
        }
    }

    // Not smaller than any incumbent (and not a tie, which would have
    // returned): append only while the sample is not yet full, mirroring bee's
    // `len < SampleSize && !added` guard.
    if sample.len() < SAMPLE_SIZE {
        sample.push(item);
    }
}

/// Build the content-addressed "reserve commitment" (RC) chunk body.
///
/// The RC chunk content is the concatenation of each sample item's
/// `chunk_address || transformed_address`, i.e. `SAMPLE_SIZE * 64` bytes
/// (bee `sampleChunk` in `pkg/storageincentives/proof.go`). This returns the
/// body only (no span header); callers BMT-hash it with span `64 * SAMPLE_SIZE`.
#[must_use]
pub fn reserve_commitment_content(items: &[SampleItem]) -> Vec<u8> {
    let mut content = Vec::with_capacity(items.len() * 64);
    for it in items {
        content.extend_from_slice(it.chunk_address.as_slice());
        content.extend_from_slice(it.transformed_address.as_slice());
    }
    content
}

/// A single chunk's inclusion proof within the proof of entitlement.
///
/// This is bee's `redistribution.ChunkInclusionProof`, the structure submitted
/// to `Redistribution.sol`. It bundles three BMT proofs for one witnessed
/// sample item:
///
/// - [`Self::rc_proof`] (`proofSegments`/`proveSegment`): the item's slot in the
///   reserve-commitment chunk.
/// - [`Self::og_proof`] (`proofSegments2`/`proveSegment2`/`chunk_span`): a plain
///   (original) BMT segment proof over the chunk's own body.
/// - [`Self::tr_proof`] (`proofSegments3`): an `anchor1`-prefixed
///   (transformed) BMT segment proof over the same body.
///
/// Postage and SOC witness data are tracked separately on [`SampleItem`].
#[derive(Clone, Debug)]
pub struct ChunkInclusionProof {
    /// Reserve-commitment chunk inclusion proof (OG of the RC chunk).
    pub rc_proof: Proof,
    /// Plain (original) BMT segment proof over the witnessed chunk body.
    pub og_proof: Proof,
    /// Anchor-prefixed (transformed) BMT segment proof over the same body.
    pub tr_proof: Proof,
    /// The little-endian `u64` span of the witnessed chunk body (`chunkSpan`).
    pub chunk_span: u64,
}

/// The three-witness proof of entitlement (bee `ChunkInclusionProofs`).
#[derive(Clone, Debug)]
pub struct ChunkInclusionProofs {
    /// Witness A, for sample item `require1`.
    pub a: ChunkInclusionProof,
    /// Witness B, for sample item `require2`.
    pub b: ChunkInclusionProof,
    /// Witness C, for sample item `require3` (always the last, `SAMPLE_SIZE-1`).
    pub c: ChunkInclusionProof,
}

/// The three witness indices selected by `anchor2` (bee require1/require2/require3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WitnessIndices {
    /// First witness: `anchor2 mod (SAMPLE_SIZE - 1)`.
    pub require1: usize,
    /// Second witness: `anchor2 mod (SAMPLE_SIZE - 2)`, bumped past `require1`.
    pub require2: usize,
    /// Third witness: always the last sample slot, `SAMPLE_SIZE - 1`.
    pub require3: usize,
    /// BMT segment index to prove: `anchor2 mod 128`.
    pub segment_index: usize,
}

/// Derive the witness indices from `anchor2` (the claim-time reserve salt).
///
/// `anchor2` is interpreted as a **big-endian** unsigned integer, exactly as
/// bee does with `new(big.Int).SetBytes(anchor2)`:
///
/// - `require1 = anchor2 mod 15`
/// - `require2 = anchor2 mod 14`, incremented by one if `>= require1` so the two
///   witnesses are distinct.
/// - `require3 = 15` (the last sample slot).
/// - `segment_index = anchor2 mod 128`.
///
/// These big-endian moduli are unrelated to the little-endian `u64` BMT spans;
/// the two must not be conflated.
#[must_use]
pub fn witness_indices(anchor2: &[u8]) -> WitnessIndices {
    let require3 = SAMPLE_SIZE - 1; // 15
    let a2 = mod_be(anchor2, require3 as u64);
    let require1 = a2 as usize;
    let mut require2 = mod_be(anchor2, (require3 - 1) as u64) as usize;
    if require2 >= require1 {
        require2 += 1;
    }
    let segment_index = mod_be(anchor2, 128) as usize;

    WitnessIndices {
        require1,
        require2,
        require3,
        segment_index,
    }
}

/// `big-endian(bytes) mod m`, computed without a big-integer dependency.
fn mod_be(bytes: &[u8], m: u64) -> u64 {
    let mut acc: u128 = 0;
    for &b in bytes {
        acc = ((acc << 8) | u128::from(b)) % u128::from(m);
    }
    acc as u64
}

/// Build the proof of entitlement for `items` from the two round anchors.
///
/// Reproduces bee's `makeInclusionProofs` (`pkg/storageincentives/proof.go`).
/// `anchor1` is the sample-time reserve salt (the BMT prefix used for the
/// transformed addresses and the TR proofs); `anchor2` is the claim-time
/// reserve salt that selects the witness indices and segment via
/// [`witness_indices`].
///
/// For each of the three witnesses it emits:
/// 1. an RC-chunk inclusion proof at segment `2 * require` (the slot holding the
///    item's *chunk* address inside the reserve-commitment chunk);
/// 2. a plain BMT segment proof at `segment_index` over the witnessed chunk's
///    body (its `chunkSpan` is the body's little-endian `u64` span);
/// 3. an `anchor1`-prefixed BMT segment proof at `segment_index` over the same
///    body.
///
/// For a SOC the body is the wrapped CAC, taken from [`SOC_SPAN_OFFSET`].
///
/// # Errors
///
/// Returns an error if `items` does not contain exactly [`SAMPLE_SIZE`]
/// elements, if either anchor is empty, or if any underlying BMT proof
/// generation fails (e.g. an out-of-range segment index).
#[allow(
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "witness indices are bee's require1/2/3 (all < SAMPLE_SIZE, checked above) and the chunk body always carries its span header"
)]
pub fn make_inclusion_proofs(
    items: &[SampleItem],
    anchor1: &[u8],
    anchor2: &[u8],
) -> core::result::Result<ChunkInclusionProofs, ProofError> {
    if items.len() != SAMPLE_SIZE {
        return Err(ProofError::SampleSize(items.len()));
    }
    if anchor1.is_empty() {
        return Err(ProofError::MissingAnchor1);
    }
    if anchor2.is_empty() {
        return Err(ProofError::MissingAnchor2);
    }

    let idx = witness_indices(anchor2);

    // Reserve-commitment chunk: a CAC over the 16 (chunk_addr || transformed)
    // pairs. Its span is the body length, 64 * SAMPLE_SIZE bytes.
    let rc_content = reserve_commitment_content(items);
    let mut rc_hasher = DefaultHasher::new();
    rc_hasher.set_span(rc_content.len() as u64);
    rc_hasher.update(&rc_content);

    let witness = |require: usize| -> core::result::Result<ChunkInclusionProof, ProofError> {
        let item = &items[require];

        // RC chunk inclusion proof at the even slot holding the chunk address.
        let rc_proof = rc_hasher.generate_proof(&rc_content, require * 2)?;

        // The witnessed chunk's own body: span||payload, skipping the SOC header.
        let offset = if item.is_soc { SOC_SPAN_OFFSET } else { 0 };
        let span = u64::from_le_bytes(
            item.chunk_data[offset..offset + SPAN_SIZE]
                .try_into()
                .expect("chunk data must contain an 8-byte span header"),
        );
        let payload = &item.chunk_data[offset + SPAN_SIZE..];

        // OG: plain BMT segment proof.
        let mut og_hasher = DefaultHasher::new();
        og_hasher.set_span(span);
        og_hasher.update(payload);
        let og_proof = og_hasher.generate_proof(payload, idx.segment_index)?;

        // TR: anchor1-prefixed BMT segment proof over the same body.
        let mut tr_hasher = DefaultHasher::with_prefix(anchor1);
        tr_hasher.set_span(span);
        tr_hasher.update(payload);
        let tr_proof = tr_hasher.generate_proof(payload, idx.segment_index)?;

        Ok(ChunkInclusionProof {
            rc_proof,
            og_proof,
            tr_proof,
            chunk_span: span,
        })
    };

    Ok(ChunkInclusionProofs {
        a: witness(idx.require1)?,
        b: witness(idx.require2)?,
        c: witness(idx.require3)?,
    })
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions over known-bounds fixtures"
)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> ChunkAddress {
        SwarmAddress::from(B256::repeat_byte(byte))
    }

    /// A CAC sample item from a span/payload body, transformed under `anchor`.
    fn cac_item(anchor: &[u8], payload: &[u8]) -> SampleItem {
        let mut data = Vec::with_capacity(SPAN_SIZE + payload.len());
        data.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        data.extend_from_slice(payload);

        // Plain address.
        let mut h = DefaultHasher::new();
        h.set_span(payload.len() as u64);
        h.update(payload);
        let chunk_address = SwarmAddress::from(h.sum());

        let transformed_address = transformed_address(anchor, &chunk_address, &data, false);
        SampleItem {
            transformed_address,
            chunk_address,
            chunk_data: data,
            is_soc: false,
            stamp: Stamp::default(),
        }
    }

    #[test]
    fn canonical_neighbourhood_filters_by_depth() {
        let anchor = SwarmAddress::zero();
        let near = addr(0x00);
        let far = addr(0xff);

        let hood = canonical_neighbourhood(&anchor, 1, [near, far]);
        assert_eq!(hood, vec![near], "depth filter must drop distant addresses");

        let all = canonical_neighbourhood(&anchor, 0, [near, far]);
        assert_eq!(all.len(), 2, "depth 0 admits every address");
    }

    #[test]
    fn canonical_neighbourhood_preserves_input_order() {
        // The function no longer sorts; it is a pure depth filter.
        let anchor = SwarmAddress::zero();
        let addrs = vec![addr(0x01), addr(0x02), addr(0x03)];
        let hood = canonical_neighbourhood(&anchor, 0, addrs.clone());
        assert_eq!(hood, addrs);
    }

    #[test]
    fn reserve_sample_keeps_smallest_transformed_addresses_in_order() {
        let anchor = b"swarm-test-anchor-deterministic!";
        let mut items = Vec::new();
        for i in 0..40u8 {
            items.push(cac_item(anchor, &[i; 20]));
        }

        let sample = reserve_sample(items.clone());
        assert_eq!(sample.len(), SAMPLE_SIZE);

        // Ascending transformed-address order.
        for w in sample.windows(2) {
            assert!(
                w[0].transformed_address.as_slice() < w[1].transformed_address.as_slice(),
                "sample must be strictly ascending by transformed address"
            );
        }

        // Exactly the 16 smallest transformed addresses overall.
        let mut all: Vec<_> = items
            .iter()
            .map(|i| i.transformed_address)
            .collect::<Vec<_>>();
        all.sort_by(|a, b| a.as_slice().cmp(b.as_slice()));
        let want: Vec<_> = all.into_iter().take(SAMPLE_SIZE).collect();
        let got: Vec<_> = sample.iter().map(|i| i.transformed_address).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn reserve_sample_order_independent() {
        let anchor = b"swarm-test-anchor-deterministic!";
        let items: Vec<_> = (0..30u8).map(|i| cac_item(anchor, &[i; 24])).collect();
        let mut reversed = items.clone();
        reversed.reverse();
        assert_eq!(reserve_sample(items), reserve_sample(reversed));
    }

    #[test]
    fn reserve_sample_tie_break_prefers_cac() {
        let anchor = b"x";
        // Two items with the same transformed address but different types.
        let base = cac_item(anchor, &[7; 16]);
        let soc_dup = SampleItem {
            is_soc: true,
            chunk_address: addr(0xaa),
            ..base.clone()
        };

        // SOC inserted first, then CAC: CAC must win the slot.
        let out = reserve_sample(vec![soc_dup.clone(), base.clone()]);
        assert_eq!(out.len(), 1);
        assert!(!out[0].is_soc, "CAC must replace SOC on a transformed tie");

        // CAC inserted first, then SOC: CAC must be retained.
        let out = reserve_sample(vec![base.clone(), soc_dup]);
        assert_eq!(out.len(), 1);
        assert!(!out[0].is_soc, "incumbent CAC must survive a SOC tie");
    }

    #[test]
    fn witness_indices_match_bee_for_anchor2_30() {
        // anchor2 = 30 (big-endian) -> bee picks 0, 3, 15 with segment 30.
        let idx = witness_indices(&[30]);
        assert_eq!(idx.require1, 0);
        assert_eq!(idx.require2, 3);
        assert_eq!(idx.require3, 15);
        assert_eq!(idx.segment_index, 30);
    }

    #[test]
    fn mod_be_is_big_endian() {
        // 0x0100 = 256; 256 mod 15 = 1.
        assert_eq!(mod_be(&[0x01, 0x00], 15), 1);
        // 0x0001 = 1; 1 mod 15 = 1.
        assert_eq!(mod_be(&[0x00, 0x01], 15), 1);
        // 256 mod 128 = 0; 256 mod 14 = 4.
        assert_eq!(mod_be(&[0x01, 0x00], 128), 0);
        assert_eq!(mod_be(&[0x01, 0x00], 14), 4);
    }
}
