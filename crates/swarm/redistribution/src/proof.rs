//! The proof of entitlement: per-witness inclusion proofs.

use nectar_primitives::bmt::Prover;
use nectar_primitives::error::PrimitivesError;
use nectar_primitives::{AnyChunk, DefaultHasher, Proof};
use vertex_swarm_postage::Stamp;

use crate::SAMPLE_SIZE;
use crate::anchor::{ClaimAnchor, SampleAnchor};
use crate::sample::{SampleItem, reserve_commitment_content};
use crate::witness::witness_indices;

/// A single chunk's inclusion proof within the proof of entitlement.
///
/// This is the structure submitted to `Redistribution.sol`. It bundles three
/// BMT proofs for one witnessed sample item plus the exact postage stamp the
/// slot was won with:
///
/// - [`Self::rc_proof`] (`proofSegments`/`proveSegment`): the item's slot in the
///   reserve-commitment chunk.
/// - [`Self::og_proof`] (`proofSegments2`/`proveSegment2`/`chunk_span`): a plain
///   (original) BMT segment proof over the chunk's own body.
/// - [`Self::tr_proof`] (`proofSegments3`): a sample-anchor-prefixed (transformed)
///   BMT segment proof over the same body.
/// - [`Self::postage_proof`]: the single [`Stamp`] the slot was won with. The
///   consensus rule is that each witness carries **exactly one** postage proof,
///   and it is the precise stamp `(batchID, 8-byte index, timestamp,
///   signature)` the candidate was selected under, taken straight off the
///   winning [`SampleItem::stamp`]. It is never re-loaded by `batchID` alone: a
///   batch holds many distinct stamps, so a batch-keyed reload could witness a
///   different stamp than the one that actually won the slot, which the on-chain
///   verifier would reject.
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
    /// The exact postage stamp the witnessed slot was won with (the witness's
    /// single `PostageProof`).
    pub postage_proof: Stamp,
}

/// The three-witness proof of entitlement.
///
/// The proofs are for the two anchor-challenged sample slots and the last slot
/// (see [`WitnessIndices`](crate::WitnessIndices)), in submission order.
#[derive(Clone, Debug)]
pub struct ChunkInclusionProofs(pub [ChunkInclusionProof; 3]);

impl ChunkInclusionProofs {
    /// The proofs for the two anchor-challenged slots, in submission order.
    #[inline]
    #[must_use]
    pub fn challenged(&self) -> [&ChunkInclusionProof; 2] {
        [&self.0[0], &self.0[1]]
    }

    /// The proof for the last (maximum) sample slot.
    #[inline]
    #[must_use]
    pub fn last(&self) -> &ChunkInclusionProof {
        &self.0[2]
    }

    /// Iterate the three witness proofs in submission order.
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
    /// A witnessed slot had no stamp pinned to it.
    ///
    /// Each witness must carry exactly one `PostageProof`: the precise stamp the
    /// slot was won with (see [`ChunkInclusionProof::postage_proof`]). A winning
    /// candidate must therefore be built with [`SampleItem::with_stamp`]; a slot
    /// reaching proof time with [`SampleItem::stamp`] unset is a construction
    /// bug, refused here rather than papered over by re-loading a stamp by
    /// `batchID` (which could witness a different stamp than the one that won).
    /// Carries the offending sample slot index.
    #[error("witnessed sample slot {0} has no stamp to prove")]
    MissingStamp(usize),
    /// A BMT proof could not be generated.
    #[error(transparent)]
    #[strum(serialize = "bmt_error")]
    Bmt(#[from] PrimitivesError),
}

/// Build the proof of entitlement for `items` from the two round anchors.
///
/// `sample` (the sample-time [`SampleAnchor`]) is the BMT prefix used for the
/// transformed addresses and the TR proofs; `claim` (the claim-time
/// [`ClaimAnchor`]) selects the witness slots and segment via
/// [`witness_indices`]. Their distinct types make a transposition a compile
/// error rather than a silent, round-losing bug.
///
/// For each opened slot it emits:
/// 1. an RC-chunk inclusion proof at segment `2 * slot` (the slot holding the
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
/// Each emitted witness also carries the exact stamp its slot was won with
/// ([`ChunkInclusionProof::postage_proof`]), read from [`SampleItem::stamp`]: it
/// is the single `PostageProof` the consensus rule requires, and it is never
/// re-loaded by `batchID` alone.
///
/// # Errors
///
/// Returns an error if `items` does not contain exactly [`SAMPLE_SIZE`]
/// elements, if a witnessed slot has no stamp pinned to it
/// ([`ProofError::MissingStamp`]), or if any underlying BMT proof generation
/// fails (e.g. an out-of-range segment index). The anchors are `bytes32` by
/// construction, so the function never has to check for an unset salt.
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

    let witness = |slot: usize| -> Result<ChunkInclusionProof, ProofError> {
        // `slot` is one of `witness_indices`' outputs, all `< SAMPLE_SIZE`, and
        // `items.len() == SAMPLE_SIZE` is enforced above, so this lookup never
        // misses. The fallible form keeps the hot path index-safe (no panic, no
        // `#[allow(indexing_slicing)]`); the `ok_or` arm is unreachable by
        // construction rather than a real `SampleSize` failure.
        debug_assert!(slot < items.len(), "witness slot out of sample bounds");
        let item = items.get(slot).ok_or(ProofError::SampleSize(items.len()))?;

        // The single PostageProof for this witness: the exact stamp the slot was
        // won with, taken straight off the winning item. Never re-loaded by
        // batchID (a batch holds many stamps), so the witnessed stamp is byte-
        // identical to the one the candidate was selected under.
        let postage_proof = item.stamp.clone().ok_or(ProofError::MissingStamp(slot))?;

        // RC chunk inclusion proof at the even slot holding the chunk address.
        let rc_proof = rc_hash.generate_proof(&rc_content, slot * 2)?;

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
            postage_proof,
        })
    };

    Ok(ChunkInclusionProofs([
        witness(idx.challenged[0])?,
        witness(idx.challenged[1])?,
        witness(idx.last)?,
    ]))
}
