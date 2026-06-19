//! The reserve sample: selection and the reserve-commitment body.

use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_postage::Stamp;

use crate::SAMPLE_SIZE;
use crate::anchor::SampleAnchor;

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
/// # The winning stamp travels with the item
///
/// A chunk's content address is stamp independent, but the reserve admits one
/// entry per distinct stamped entry `(batchID, 8-byte stampIndex, address)`, and
/// the consensus rule is that each inclusion-proof witness must carry the
/// **exact** stamp the sample slot was won with: the precise `(batchID,
/// stampIndex, timestamp, signature)`, not a stamp re-loaded by `batchID` alone
/// (a batch holds many distinct stamps; re-loading by batch could witness a
/// different one). [`Self::stamp`] therefore pins that identity to the slot from
/// the moment the candidate is selected, and [`make_inclusion_proofs`] reads it
/// straight off the winning item.
///
/// [`make_inclusion_proofs`]: crate::make_inclusion_proofs
///
/// The stamp is an [`Option`] only because the consensus *ordering* primitives
/// ([`reserve_sample`], [`reserve_commitment_content`]) and their byte-for-byte
/// reference vectors are stamp independent: those fixtures exercise the
/// transformed-address geometry and carry no stamp. A real candidate feed always
/// builds items with [`Self::with_stamp`]; a `None` stamp surfaces as a
/// [`ProofError::MissingStamp`](crate::ProofError::MissingStamp) the moment a
/// proof of entitlement is built for that slot, so an unstamped item can never
/// silently win a round.
///
/// [`reserve_commitment_content`]: crate::reserve_commitment_content
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleItem {
    /// The anchor-keyed transformed address the sample is ordered by.
    pub transformed_address: ChunkAddress,
    /// The typed chunk (content or single-owner) this item witnesses.
    pub chunk: AnyChunk,
    /// The exact stamp this slot was won with: the precise `(batchID,
    /// stampIndex, timestamp, signature)` carried straight into the inclusion
    /// proof. `None` only for the stamp-independent ordering fixtures (see the
    /// type-level note); a candidate feed always sets it.
    pub stamp: Option<Stamp>,
}

impl SampleItem {
    /// Build a stamp-independent sample item for `chunk` under the sample-time
    /// anchor.
    ///
    /// The transformed address is computed by nectar's
    /// [`transformed_address`](AnyChunk::transformed_address). This constructor
    /// leaves [`Self::stamp`] unset and exists for the consensus *ordering*
    /// vectors, which are stamp independent. Production candidate feeds use
    /// [`Self::with_stamp`] so the winning stamp travels with the slot.
    #[must_use]
    pub fn new(sample: SampleAnchor, chunk: AnyChunk) -> Self {
        Self {
            transformed_address: chunk.transformed_address(sample.as_bytes()),
            chunk,
            stamp: None,
        }
    }

    /// Build a sample item carrying the exact `stamp` the slot was won with.
    ///
    /// This is the candidate-feed constructor: it pins the precise stamp
    /// identity to the slot so the proof of entitlement witnesses that stamp and
    /// no other (see the type-level note).
    #[must_use]
    pub fn with_stamp(sample: SampleAnchor, chunk: AnyChunk, stamp: Stamp) -> Self {
        Self {
            transformed_address: chunk.transformed_address(sample.as_bytes()),
            chunk,
            stamp: Some(stamp),
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
/// On a transformed-address tie the protocol keeps the **content-addressed**
/// chunk (the equal-address branch replaces the incumbent only when the new item
/// is *not* a valid SOC), so that the on-chain ordering check cannot be gamed by
/// a single-owner chunk colliding with a CAC. We reproduce that exact tie-break.
///
/// # Determinism and the same-content multi-batch tie
///
/// `candidates` may be supplied in any order: the set of selected slots and the
/// committed reserve-commitment bytes depend only on the transformed addresses,
/// not on insertion order.
///
/// One subtlety is consensus relevant. The reserve admits one entry per distinct
/// stamped entry `(batchID, stampIndex, address)`, so the *same content* may be
/// presented under several batches. Those entries share a content address (it is
/// stamp independent) and therefore a transformed address, so they tie, and "at
/// most once" collapses them to a single slot. When two such CAC entries tie the
/// equal-address branch keeps the **last** CAC seen, so *which batch's stamp*
/// travels into [`SampleItem::stamp`] (and hence the witness's single
/// `PostageProof`) is insertion-order dependent.
///
/// This matches bee, whose sampler assembles items off a concurrent worker pool
/// and resolves the same tie by last-CAC-wins, so the kept stamp is likewise not
/// order-defined there. It is consensus-safe because:
///
/// 1. The value committed on chain is the reserve-commitment hash over each
///    slot's `chunk_address || transformed_address`
///    ([`reserve_commitment_content`]). Both halves are identical for every tied
///    entry (same content => same chunk address and, under one anchor, the same
///    transformed address), so the commitment is byte-identical regardless of
///    which batch wins the slot.
/// 2. A witness's `PostageProof` is verified standalone by `Redistribution.sol`:
///    the stamp signature must bind that chunk address and the batch must be a
///    valid, funded batch. *Any* owning batch's stamp satisfies that check, so
///    the contract accepts whichever tied stamp the slot carries.
///
/// The tie-break is therefore not extended to pick a canonical stamp: doing so
/// would diverge from bee for no on-chain benefit, since the committed bytes are
/// stable and the chain binds the stamp to the chunk, not to a canonical batch.
/// (Two *different* contents colliding on a transformed address would commit
/// different `chunk_address` bytes, but that is a 256-bit HMAC-keyed BMT
/// collision, not a reachable consensus case; bee resolves it the same way.)
#[must_use]
pub fn reserve_sample(candidates: impl IntoIterator<Item = SampleItem>) -> Vec<SampleItem> {
    let mut sample: Vec<SampleItem> = Vec::with_capacity(SAMPLE_SIZE + 1);

    for item in candidates {
        insert_sample_item(&mut sample, item);
    }

    sample
}

/// Insert `item` into the running sorted sample, mirroring the canonical
/// sorted-insert semantics.
fn insert_sample_item(sample: &mut Vec<SampleItem>, item: SampleItem) {
    let key = item.transformed_address;

    // First slot whose transformed address is not strictly smaller than `key`:
    // either a tie or the insertion point.
    let Some(pos) = sample
        .iter()
        .position(|s| s.transformed_address.as_slice() >= key.as_slice())
    else {
        // Larger than every incumbent: append only while the sample is not yet
        // full, mirroring the canonical append-only-while-not-full guard.
        if sample.len() < SAMPLE_SIZE {
            sample.push(item);
        }
        return;
    };

    match sample.get_mut(pos) {
        // Tie on the transformed address: the incumbent is replaced only when
        // the new chunk is a CAC (not a valid SOC), so a CAC always wins the
        // slot. Either way no new slot is consumed. For two CACs of the same
        // content under different batches this keeps the last CAC seen, so the
        // kept stamp is insertion-order dependent; the committed bytes are
        // identical regardless and the chain binds the stamp to the chunk, so
        // this is consensus-safe (see the function-level note, and it mirrors
        // bee's concurrent last-CAC-wins assembly).
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test assertions over known-bounds fixtures"
)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use nectar_primitives::DefaultContentChunk;

    /// A CAC sample item from a payload, transformed under `anchor`.
    fn cac_item(anchor: SampleAnchor, payload: &[u8]) -> SampleItem {
        let chunk = DefaultContentChunk::new(payload.to_vec()).unwrap();
        SampleItem::new(anchor, chunk.into())
    }

    /// A deterministic 32-byte sample-time anchor.
    fn sample_anchor() -> SampleAnchor {
        SampleAnchor::new(B256::from_slice(b"swarm-test-anchor-deterministic!"))
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
}
