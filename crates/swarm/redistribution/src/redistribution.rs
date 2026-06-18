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
//! the inclusion proofs must match Swarm's reference implementation byte for
//! byte, because the same values are checked on chain by the
//! `Redistribution.sol` storage-incentives contract. Any divergence makes
//! vertex lose (or be slashed in) the redistribution round. The Go reference
//! lives in bee's `pkg/storer/sample.go` (sampling) and
//! `pkg/storageincentives/proof.go` (proof of entitlement); the Rust here
//! mirrors them deliberately and is validated against bee's published vectors
//! in `tests/`.
//!
//! The anchor-keyed transformed address itself (the value the sample is ordered
//! by) is a nectar primitive: [`AnyChunk::transformed_address`]. nectar owns
//! that parity oracle, so this crate consumes it rather than re-deriving it.
//!
//! The building blocks are:
//!
//! - [`SampleAnchor`] / [`ClaimAnchor`] carry the two non-empty per-round
//!   reserve salts as distinct types, so they cannot be transposed.
//! - [`canonical_neighbourhood`] filters chunk addresses to those a node is
//!   responsible for at a given committed depth.
//! - [`SampleItem`] / [`reserve_sample`] select the [`SAMPLE_SIZE`] chunks with
//!   the lexicographically smallest transformed addresses.
//! - [`make_inclusion_proofs`] / [`ChunkInclusionProof`] build the proof of
//!   entitlement (the witness data submitted to the contract).

use core::fmt;

use alloy_primitives::B256;
use nectar_primitives::bmt::Prover;
use nectar_primitives::error::PrimitivesError;
use nectar_primitives::{AnyChunk, Bin, ChunkAddress, DefaultHasher, Proof, SwarmAddress};

/// Number of chunks retained in a reserve sample (the reference `SampleSize`).
pub const SAMPLE_SIZE: usize = 16;

// =============================================================================
// Round anchors
// =============================================================================

/// The sample-time reserve salt (the reference's `anchor1`).
///
/// The `bytes32 currentRoundAnchor` read from `Redistribution.sol`, used as the
/// BMT prefix that keys transformed addresses (via
/// [`AnyChunk::transformed_address`]) and the transformed (TR) inclusion proof.
/// Fixed-width `B256` because the on-chain anchor is always a `bytes32` (bee's
/// `ReserveSalt` unpacks a `[32]byte`); the earlier vertex-internal `&[u8]`
/// over-fit to the minimal-length anchors the in-tree reference *test* uses.
///
/// A distinct type from [`ClaimAnchor`] so the two round salts — which play
/// structurally different roles and must never be transposed (a swap yields
/// garbage proofs and a lost round) — cannot be passed to each other's slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SampleAnchor(B256);

impl SampleAnchor {
    /// Wrap the on-chain sample-time anchor (`bytes32`).
    #[must_use]
    pub const fn new(anchor: B256) -> Self {
        Self(anchor)
    }

    /// The raw salt bytes, threaded untouched into the hashing primitives.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl From<B256> for SampleAnchor {
    fn from(anchor: B256) -> Self {
        Self(anchor)
    }
}

/// The claim-time reserve salt (the reference's `anchor2`).
///
/// The `bytes32 currentRoundAnchor`, interpreted big-endian to select the three
/// witness indices and the proven segment (see [`witness_indices`]). See
/// [`SampleAnchor`] for why this is a fixed-width `B256` and a distinct type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClaimAnchor(B256);

impl ClaimAnchor {
    /// Wrap the on-chain claim-time anchor (`bytes32`).
    #[must_use]
    pub const fn new(anchor: B256) -> Self {
        Self(anchor)
    }

    /// The raw salt bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl From<B256> for ClaimAnchor {
    fn from(anchor: B256) -> Self {
        Self(anchor)
    }
}

// =============================================================================
// Committed depth
// =============================================================================

/// The committed neighbourhood depth for a redistribution round: the boundary
/// at which a storer's reserve is sampled.
///
/// Chunks whose proximity to the round anchor meets this depth are the node's
/// committed sample neighbourhood. It is a distinguished [`Bin`] in a
/// redistribution-specific role. This is intentionally a distinct type from
/// vertex's routing [`NeighborhoodDepth`][nd]: that type is the local
/// connectivity boundary (supply-side, local-only), whereas this is the
/// per-round, on-chain-derived reserve-commitment depth. The bytes are a plain
/// `u8 >= u8` compare either way; the separate type keeps the roles from being
/// conflated.
///
/// [nd]: vertex_swarm_primitives::NeighborhoodDepth
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommittedDepth(Bin);

impl CommittedDepth {
    /// The shallowest depth (every address is in the neighbourhood).
    pub const ZERO: Self = Self(Bin::ZERO);

    /// Wrap a [`Bin`] as a committed depth.
    #[must_use]
    pub const fn new(bin: Bin) -> Self {
        Self(bin)
    }

    /// The boundary as a [`Bin`].
    #[must_use]
    pub const fn bin(self) -> Bin {
        self.0
    }

    /// The raw boundary index. For edges only (logs, metrics, the wire).
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0.get()
    }

    /// Whether `bin` is inside the committed neighbourhood (`bin >= depth`).
    ///
    /// O(1): a single `u8` comparison, no iteration or allocation despite the
    /// set-like name.
    #[must_use]
    pub fn contains(self, bin: Bin) -> bool {
        bin >= self.0
    }
}

impl fmt::Display for CommittedDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "depth={}", self.0.get())
    }
}

impl TryFrom<u8> for CommittedDepth {
    type Error = nectar_primitives::BinError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Bin::try_from(value).map(Self)
    }
}

/// The deterministic neighbourhood for `anchor` at the given committed `depth`.
///
/// Returns the subset of `addrs` a node is responsible for, i.e. those whose
/// proximity order to `anchor` is at least `depth`. A [`CommittedDepth::ZERO`]
/// depth admits every address.
///
/// Unlike an earlier vertex-internal implementation this does **not** impose an
/// XOR-distance ordering: the reference never orders the neighbourhood by
/// distance, it streams the depth-filtered chunks and orders the *sample* by
/// transformed address (see [`reserve_sample`]). Imposing an extra distance sort
/// here would be dead work at best and a parity hazard at worst, so the
/// membership filter is all this function does. Callers that need a sample must
/// feed the result (or any iteration order) into [`reserve_sample`], whose
/// output order is fixed by the transformed addresses and therefore independent
/// of input order.
///
/// # Examples
///
/// ```
/// use vertex_swarm_redistribution::{CommittedDepth, canonical_neighbourhood};
/// use nectar_primitives::SwarmAddress;
/// use alloy_primitives::B256;
///
/// let anchor = SwarmAddress::zero();
/// let near = SwarmAddress::from(B256::ZERO);
/// let far = SwarmAddress::from(B256::repeat_byte(0xff));
/// let depth = CommittedDepth::try_from(1).unwrap();
/// let hood = canonical_neighbourhood(&anchor, depth, [near, far]);
/// assert_eq!(hood, vec![near]);
/// ```
#[must_use]
pub fn canonical_neighbourhood(
    anchor: &SwarmAddress,
    depth: CommittedDepth,
    addrs: impl IntoIterator<Item = ChunkAddress>,
) -> Vec<ChunkAddress> {
    addrs
        .into_iter()
        .filter(|addr| depth.contains(Bin::from(anchor.proximity(addr))))
        .collect()
}

// =============================================================================
// Reserve sample
// =============================================================================

/// A single entry in a reserve sample.
///
/// It carries the typed chunk so that the proof of entitlement can re-derive
/// both the original (OG) and transformed (TR) BMT proofs without re-parsing
/// raw bytes or branching on a chunk-type boolean: an [`AnyChunk`] already
/// knows whether it is a CAC or a SOC, and its
/// [`span`](AnyChunk::span)/[`data`](AnyChunk::data) accessors delegate to the
/// inner BMT body for both, so SOC witness reads need no `id`/`signature`
/// header slicing.
///
/// The postage-stamp witness is intentionally absent: it is not built here.
/// Reintroduce it via `nectar_postage::Stamp` only when that witness lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleItem {
    /// The anchor-keyed transformed address the sample is ordered by.
    pub transformed_address: ChunkAddress,
    /// The typed chunk (content or single-owner) this item witnesses.
    pub chunk: AnyChunk,
}

impl SampleItem {
    /// Build a sample item for `chunk` under the sample-time anchor.
    ///
    /// The transformed address is computed by nectar's
    /// [`AnyChunk::transformed_address`], the byte-for-byte parity oracle.
    #[must_use]
    pub fn new(sample: SampleAnchor, chunk: AnyChunk) -> Self {
        Self {
            transformed_address: chunk.transformed_address(sample.as_bytes()),
            chunk,
        }
    }

    /// The chunk's own (content or single-owner) address.
    #[must_use]
    pub fn chunk_address(&self) -> &ChunkAddress {
        self.chunk.address()
    }
}

/// Select the reserve sample from `candidates`.
///
/// Keeps the [`SAMPLE_SIZE`] chunks with the lexicographically smallest
/// transformed addresses, returned in ascending transformed-address order. This
/// is a sorted insertion that drops the largest element once the sample is full.
///
/// On a transformed-address tie the reference keeps the **content-addressed**
/// chunk (the equal-address branch replaces the incumbent only when the new item
/// is *not* a valid SOC), so that the on-chain ordering check cannot be gamed by
/// a single-owner chunk colliding with a CAC. We reproduce that exact tie-break.
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

/// Insert `item` into the running sorted sample, mirroring the reference
/// `insert` semantics.
fn insert_sample_item(sample: &mut Vec<SampleItem>, item: SampleItem) {
    let key = item.transformed_address;

    // First slot whose transformed address is not strictly smaller than `key`:
    // either a tie or the insertion point.
    let Some(pos) = sample
        .iter()
        .position(|s| s.transformed_address.as_slice() >= key.as_slice())
    else {
        // Larger than every incumbent: append only while the sample is not yet
        // full, mirroring the reference `len < SampleSize && !added` guard.
        if sample.len() < SAMPLE_SIZE {
            sample.push(item);
        }
        return;
    };

    match sample.get_mut(pos) {
        // Tie on the transformed address: the incumbent is replaced only when
        // the new chunk is a CAC (not a valid SOC), so a CAC always wins the
        // slot. Either way no new slot is consumed.
        Some(incumbent) if incumbent.transformed_address == key => {
            if item.chunk.is_content() {
                *incumbent = item;
            }
        }
        // Strictly smaller than the incumbent at `pos`: insert before it.
        _ => {
            sample.insert(pos, item);
            // Trim to SampleSize after a sorted insertion (the slice re-append
            // can transiently overshoot by one).
            if sample.len() > SAMPLE_SIZE {
                sample.truncate(SAMPLE_SIZE);
            }
        }
    }
}

/// Build the content-addressed "reserve commitment" (RC) chunk body.
///
/// The RC chunk content is the concatenation of each sample item's
/// `chunk_address || transformed_address`, i.e. `SAMPLE_SIZE * 64` bytes. This
/// returns the body only (no span header); callers BMT-hash it with span
/// `64 * SAMPLE_SIZE`.
#[must_use]
pub fn reserve_commitment_content(items: &[SampleItem]) -> Vec<u8> {
    let mut content = Vec::with_capacity(items.len() * 64);
    for it in items {
        content.extend_from_slice(it.chunk_address().as_slice());
        content.extend_from_slice(it.transformed_address.as_slice());
    }
    content
}

// =============================================================================
// Proof of entitlement
// =============================================================================

/// A single chunk's inclusion proof within the proof of entitlement.
///
/// This is the structure submitted to `Redistribution.sol`. It bundles three
/// BMT proofs for one witnessed sample item:
///
/// - [`Self::rc_proof`] (`proofSegments`/`proveSegment`): the item's slot in the
///   reserve-commitment chunk.
/// - [`Self::og_proof`] (`proofSegments2`/`proveSegment2`/`chunk_span`): a plain
///   (original) BMT segment proof over the chunk's own body.
/// - [`Self::tr_proof`] (`proofSegments3`): a sample-anchor-prefixed (transformed)
///   BMT segment proof over the same body.
///
/// Postage and SOC witness data are tracked separately.
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

/// The three-witness proof of entitlement.
///
/// The three proofs are for sample items `require1`/`require2`/`require3` (the
/// witnesses selected by the claim anchor; see [`WitnessIndices`]), in that order.
#[derive(Clone, Debug)]
pub struct ChunkInclusionProofs(pub [ChunkInclusionProof; 3]);

impl ChunkInclusionProofs {
    /// Iterate the three witness proofs in `require1`/`require2`/`require3` order.
    pub fn iter(&self) -> core::slice::Iter<'_, ChunkInclusionProof> {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a ChunkInclusionProofs {
    type Item = &'a ChunkInclusionProof;
    type IntoIter = core::slice::Iter<'a, ChunkInclusionProof>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// The three witness indices selected by the claim anchor (require1/require2/require3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WitnessIndices {
    /// First witness: `claim mod (SAMPLE_SIZE - 1)`.
    pub require1: usize,
    /// Second witness: `claim mod (SAMPLE_SIZE - 2)`, bumped past `require1`.
    pub require2: usize,
    /// Third witness: always the last sample slot, `SAMPLE_SIZE - 1`.
    pub require3: usize,
    /// BMT segment index to prove: `claim mod 128`.
    pub segment_index: usize,
}

/// Derive the witness indices from the claim-time anchor.
///
/// The claim anchor is interpreted as a **big-endian** unsigned integer:
///
/// - `require1 = claim mod 15`
/// - `require2 = claim mod 14`, incremented by one if `>= require1` so the two
///   witnesses are distinct.
/// - `require3 = 15` (the last sample slot).
/// - `segment_index = claim mod 128`.
///
/// These big-endian moduli are unrelated to the little-endian `u64` BMT spans;
/// the two must not be conflated.
#[must_use]
pub fn witness_indices(claim: ClaimAnchor) -> WitnessIndices {
    let bytes = claim.as_bytes();
    let require3 = SAMPLE_SIZE - 1; // 15
    let require1 = mod_be(bytes, require3 as u64) as usize;
    let mut require2 = mod_be(bytes, (require3 - 1) as u64) as usize;
    if require2 >= require1 {
        require2 += 1;
    }
    let segment_index = mod_be(bytes, 128) as usize;

    WitnessIndices {
        require1,
        require2,
        require3,
        segment_index,
    }
}

/// `big-endian(bytes) mod m`, computed without a big-integer dependency.
///
/// The `u128` accumulator never overflows: after each `% m` it is `< m <= u64`,
/// so shifting in one more byte stays within `u128`.
fn mod_be(bytes: &[u8], m: u64) -> u64 {
    let mut acc: u128 = 0;
    for &b in bytes {
        acc = ((acc << 8) | u128::from(b)) % u128::from(m);
    }
    acc as u64
}

/// A BMT-hashable body: a little-endian `u64` span plus its payload.
///
/// Private borrowing view that exists only to stop decomposing chunks into bare
/// `(span, payload)` pairs at internal boundaries. `span` stays a `u64` (the
/// workspace convention: `Hasher::set_span` and `Proof.span` are both bare
/// `u64`); what this type removes is the *pairing* smell, by reading both halves
/// from a single typed source.
struct Body<'a> {
    span: u64,
    payload: &'a [u8],
}

impl<'a> Body<'a> {
    /// The body of a typed chunk: its span and payload, read straight from the
    /// chunk's BMT-body accessors (identical bytes for a CAC or a SOC, since the
    /// typed accessors already skip any SOC `id`/`signature` header).
    fn of(chunk: &'a AnyChunk) -> Self {
        let payload: &'a [u8] = chunk.data();
        Self {
            span: chunk.span(),
            payload,
        }
    }

    /// Build the hasher for this body, applying `prefix` when given.
    fn hasher(&self, prefix: Option<&[u8]>) -> DefaultHasher {
        let mut hasher = match prefix {
            Some(p) => DefaultHasher::with_prefix(p),
            None => DefaultHasher::new(),
        };
        hasher.set_span(self.span);
        hasher.update(self.payload);
        hasher
    }
}

/// Errors arising while building a proof of entitlement.
#[non_exhaustive]
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ProofError {
    /// The sample did not contain exactly [`SAMPLE_SIZE`] items.
    #[error("reserve sample must have {SAMPLE_SIZE} items, got {0}")]
    SampleSize(usize),
    /// A BMT proof could not be generated.
    #[error(transparent)]
    #[strum(serialize = "bmt_error")]
    Bmt(#[from] PrimitivesError),
}

/// Build the proof of entitlement for `items` from the two round anchors.
///
/// Reproduces the reference `makeInclusionProofs`. `sample` (the sample-time
/// [`SampleAnchor`]) is the BMT prefix used for the transformed addresses and
/// the TR proofs; `claim` (the claim-time [`ClaimAnchor`]) selects the witness
/// indices and segment via [`witness_indices`]. Their distinct types make a
/// transposition a compile error rather than a silent, round-losing bug.
///
/// For each of the three witnesses it emits:
/// 1. an RC-chunk inclusion proof at segment `2 * require` (the slot holding the
///    item's *chunk* address inside the reserve-commitment chunk);
/// 2. a plain BMT segment proof at `segment_index` over the witnessed chunk's
///    body (its `chunk_span` is the body's little-endian `u64` span);
/// 3. a sample-anchor-prefixed BMT segment proof at `segment_index` over the
///    same body.
///
/// The witnessed body is read straight from the typed chunk: [`AnyChunk::span`]
/// and [`AnyChunk::data`] already delegate to the inner BMT body for both CAC
/// and SOC, so a SOC needs no `id`/`signature` header slicing.
///
/// # Errors
///
/// Returns an error if `items` does not contain exactly [`SAMPLE_SIZE`]
/// elements, or if any underlying BMT proof generation fails (e.g. an
/// out-of-range segment index). The anchors are `bytes32` by construction, so
/// the function never has to check for an unset salt.
pub fn make_inclusion_proofs(
    items: &[SampleItem],
    sample: SampleAnchor,
    claim: ClaimAnchor,
) -> Result<ChunkInclusionProofs, ProofError> {
    if items.len() != SAMPLE_SIZE {
        return Err(ProofError::SampleSize(items.len()));
    }

    let idx = witness_indices(claim);

    // Reserve-commitment chunk: a CAC over the 16 (chunk_addr || transformed)
    // pairs. Its span is the body length, 64 * SAMPLE_SIZE bytes.
    let rc_content = reserve_commitment_content(items);
    let rc_body = Body {
        span: rc_content.len() as u64,
        payload: &rc_content[..],
    };
    let rc_hash = rc_body.hasher(None);

    let witness = |require: usize| -> Result<ChunkInclusionProof, ProofError> {
        let item = items
            .get(require)
            .ok_or(ProofError::SampleSize(items.len()))?;

        // RC chunk inclusion proof at the even slot holding the chunk address.
        let rc_proof = rc_hash.generate_proof(&rc_content, require * 2)?;

        // The witnessed chunk's own body. For a SOC the typed accessors already
        // expose the wrapped CAC's span/payload, so there is no header slicing.
        let body = Body::of(&item.chunk);

        // OG: plain BMT segment proof.
        let og_proof = body
            .hasher(None)
            .generate_proof(body.payload, idx.segment_index)?;

        // TR: sample-anchor-prefixed BMT segment proof over the same body.
        let tr_proof = body
            .hasher(Some(sample.as_bytes()))
            .generate_proof(body.payload, idx.segment_index)?;

        Ok(ChunkInclusionProof {
            rc_proof,
            og_proof,
            tr_proof,
            chunk_span: body.span,
        })
    };

    Ok(ChunkInclusionProofs([
        witness(idx.require1)?,
        witness(idx.require2)?,
        witness(idx.require3)?,
    ]))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test assertions over known-bounds fixtures"
)]
mod tests {
    use super::*;
    use nectar_primitives::DefaultContentChunk;

    fn addr(byte: u8) -> ChunkAddress {
        SwarmAddress::from(alloy_primitives::B256::repeat_byte(byte))
    }

    fn depth(n: u8) -> CommittedDepth {
        CommittedDepth::try_from(n).unwrap()
    }

    /// A CAC sample item from a payload, transformed under `anchor`.
    fn cac_item(anchor: SampleAnchor, payload: &[u8]) -> SampleItem {
        let chunk = DefaultContentChunk::new(payload.to_vec()).unwrap();
        SampleItem::new(anchor, chunk.into())
    }

    /// The published 32-byte parity anchor, as a sample-time anchor.
    fn sample_anchor() -> SampleAnchor {
        SampleAnchor::new(B256::from_slice(b"swarm-test-anchor-deterministic!"))
    }

    #[test]
    fn canonical_neighbourhood_filters_by_depth() {
        let anchor = SwarmAddress::zero();
        let near = addr(0x00);
        let far = addr(0xff);

        let hood = canonical_neighbourhood(&anchor, depth(1), [near, far]);
        assert_eq!(hood, vec![near], "depth filter must drop distant addresses");

        let all = canonical_neighbourhood(&anchor, depth(0), [near, far]);
        assert_eq!(all.len(), 2, "depth 0 admits every address");
    }

    #[test]
    fn canonical_neighbourhood_preserves_input_order() {
        // The function no longer sorts; it is a pure depth filter.
        let anchor = SwarmAddress::zero();
        let addrs = vec![addr(0x01), addr(0x02), addr(0x03)];
        let hood = canonical_neighbourhood(&anchor, depth(0), addrs.clone());
        assert_eq!(hood, addrs);
    }

    #[test]
    fn reserve_sample_keeps_smallest_transformed_addresses_in_order() {
        let anchor = sample_anchor();
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
        let anchor = sample_anchor();
        let items: Vec<_> = (0..30u8).map(|i| cac_item(anchor, &[i; 24])).collect();
        let mut reversed = items.clone();
        reversed.reverse();
        assert_eq!(reserve_sample(items), reserve_sample(reversed));
    }

    #[test]
    fn reserve_sample_tie_break_prefers_cac() {
        let anchor = SampleAnchor::new(B256::repeat_byte(0xaa));
        // A CAC and a fabricated SOC sharing the same transformed address but
        // differing in type; the CAC must always win the slot.
        let base = cac_item(anchor, &[7; 16]);
        let soc_dup = SampleItem {
            chunk: soc_chunk(),
            ..base.clone()
        };

        // SOC inserted first, then CAC: CAC must win the slot.
        let out = reserve_sample(vec![soc_dup.clone(), base.clone()]);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].chunk.is_content(),
            "CAC must replace SOC on a transformed tie"
        );

        // CAC inserted first, then SOC: CAC must be retained.
        let out = reserve_sample(vec![base.clone(), soc_dup]);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].chunk.is_content(),
            "incumbent CAC must survive a SOC tie"
        );
    }

    /// A deterministic single-owner chunk for the tie-break test.
    fn soc_chunk() -> AnyChunk {
        use nectar_primitives::DefaultSingleOwnerChunk;
        let signer = alloy_signer_local::PrivateKeySigner::from_slice(&[0x42u8; 32]).unwrap();
        let soc = DefaultSingleOwnerChunk::new(
            alloy_primitives::B256::ZERO,
            b"single owner payload".to_vec(),
            &signer,
        )
        .unwrap();
        soc.into()
    }

    #[test]
    fn witness_indices_match_for_claim_value_30() {
        // claim anchor = 30 as a bytes32 (big-endian) -> 0, 3, 15 with segment 30.
        let claim = ClaimAnchor::new(B256::left_padding_from(&[30]));
        let idx = witness_indices(claim);
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

    #[test]
    fn committed_depth_round_trips_u8() {
        assert_eq!(CommittedDepth::try_from(0).unwrap().get(), 0);
        assert_eq!(CommittedDepth::try_from(31).unwrap().get(), 31);
        assert!(CommittedDepth::try_from(32).is_err());
        assert_eq!(CommittedDepth::ZERO, CommittedDepth::try_from(0).unwrap());
    }
}
