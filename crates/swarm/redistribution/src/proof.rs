//! The proof of entitlement: per-witness inclusion proofs.

use nectar_primitives::bmt::Prover;
use nectar_primitives::error::PrimitivesError;
use nectar_primitives::{AnyChunk, DefaultHasher, Proof};
use vertex_swarm_postage::Stamp;

use crate::SAMPLE_SIZE;
use crate::anchor::{ClaimAnchor, SampleAnchor};
use crate::sample::{SampleItem, reserve_commitment_content};
use crate::witness::witness_indices;

/// One witnessed sample item's inclusion proof, as submitted on-chain: three
/// BMT proofs plus the exact stamp the slot was won with.
#[derive(Clone, Debug)]
pub struct ChunkInclusionProof {
    /// Reserve-commitment chunk inclusion proof.
    pub rc_proof: Proof,
    /// Plain (original) BMT segment proof over the witnessed chunk body.
    pub og_proof: Proof,
    /// Anchor-prefixed (transformed) BMT segment proof over the same body.
    pub tr_proof: Proof,
    /// Little-endian `u64` span of the witnessed chunk body.
    pub chunk_span: u64,
    /// The exact stamp the witnessed slot was won with.
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
struct Body<'a> {
    span: u64,
    payload: &'a [u8],
}

impl<'a> Body<'a> {
    /// Span and payload from a chunk's BMT-body accessors. The typed accessors
    /// expose identical bytes for a CAC or a SOC (the SOC `id`/`signature`
    /// header is already skipped).
    fn of(chunk: &'a AnyChunk) -> Self {
        let payload: &'a [u8] = chunk.data();
        Self {
            span: chunk.span(),
            payload,
        }
    }

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
    /// A witnessed slot had no stamp pinned to it. A winning candidate must be
    /// built with [`SampleItem::with_stamp`]; an unset [`SampleItem::stamp`] at
    /// proof time is a construction bug, refused rather than reloading a stamp
    /// by `batchID`. Carries the offending sample slot index.
    #[error("witnessed sample slot {0} has no stamp to prove")]
    MissingStamp(usize),
    /// A BMT proof could not be generated.
    #[error(transparent)]
    #[strum(serialize = "bmt_error")]
    Bmt(#[from] PrimitivesError),
}

/// Build the proof of entitlement for `items` from the two round anchors.
///
/// `sample` is the BMT prefix for the transformed addresses and TR proofs;
/// `claim` selects the witness slots and segment via [`witness_indices`]. The
/// distinct anchor types make a transposition a compile error.
///
/// For each opened slot it emits an RC-chunk inclusion proof at segment
/// `2 * slot`, a plain BMT segment proof at `segment_index` over the chunk
/// body, and a sample-anchor-prefixed proof at the same index over that body.
///
/// # Errors
///
/// Errors if `items` is not exactly [`SAMPLE_SIZE`] elements, if a witnessed
/// slot has no stamp ([`ProofError::MissingStamp`]), or if BMT proof generation
/// fails (e.g. an out-of-range segment index).
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
        // `slot` is a `witness_indices` output (`< SAMPLE_SIZE`) and
        // `items.len() == SAMPLE_SIZE` is enforced above, so the `ok_or` arm is
        // unreachable; the fallible form just keeps the hot path panic-free.
        debug_assert!(slot < items.len(), "witness slot out of sample bounds");
        let item = items.get(slot).ok_or(ProofError::SampleSize(items.len()))?;

        let postage_proof = item.stamp.clone().ok_or(ProofError::MissingStamp(slot))?;

        // RC chunk inclusion proof at the even slot holding the chunk address.
        let rc_proof = rc_hash.generate_proof(&rc_content, slot * 2)?;

        let body = Body::of(&item.chunk);

        let og_proof = body
            .hasher(None)
            .generate_proof(body.payload, idx.segment_index)?;

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
