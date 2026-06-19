//! The reserve sample: selection and the reserve-commitment body.

use nectar_primitives::{AnyChunk, ChunkAddress};

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
    /// [`transformed_address`](AnyChunk::transformed_address).
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
/// On a transformed-address tie the protocol keeps the **content-addressed**
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
