//! The reserve sample: selection and the reserve-commitment body.

use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_postage::Stamp;

use crate::SAMPLE_SIZE;
use crate::anchor::SampleAnchor;

/// A single entry in a reserve sample.
///
/// Carries the typed chunk so the proof of entitlement can re-derive both the
/// original and transformed BMT proofs without re-parsing raw bytes.
///
/// [`Self::stamp`] pins the exact stamp the slot was won with
/// (`batchID, stampIndex, timestamp, signature`), since a batch holds many
/// distinct stamps and the witness must carry the winning one. It is [`Option`]
/// only because the ordering primitives ([`reserve_sample`],
/// [`reserve_commitment_content`]) are stamp independent; a `None` stamp fails as
/// [`ProofError::MissingStamp`](crate::ProofError::MissingStamp) at proof build,
/// so an unstamped item can never silently win a round.
///
/// [`reserve_commitment_content`]: crate::reserve_commitment_content
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleItem {
    /// The anchor-keyed transformed address the sample is ordered by.
    pub transformed_address: ChunkAddress,
    /// The typed chunk (content or single-owner) this item witnesses.
    pub chunk: AnyChunk,
    /// The exact stamp this slot was won with, carried into the inclusion proof.
    /// `None` only for the stamp-independent ordering fixtures.
    pub stamp: Option<Stamp>,
}

impl SampleItem {
    /// Build a stamp-independent sample item, for the consensus ordering
    /// vectors. Production candidate feeds use [`Self::with_stamp`].
    #[must_use]
    pub fn new(sample: SampleAnchor, chunk: AnyChunk) -> Self {
        Self {
            transformed_address: chunk.transformed_address(sample.as_bytes()),
            chunk,
            stamp: None,
        }
    }

    /// Build a sample item carrying the exact `stamp` the slot was won with, so
    /// the proof of entitlement witnesses that stamp and no other.
    #[must_use]
    pub fn with_stamp(sample: SampleAnchor, chunk: AnyChunk, stamp: Stamp) -> Self {
        Self {
            transformed_address: chunk.transformed_address(sample.as_bytes()),
            chunk,
            stamp: Some(stamp),
        }
    }

    #[must_use]
    pub fn chunk_address(&self) -> &ChunkAddress {
        self.chunk.address()
    }
}

/// Select the reserve sample from `candidates`.
///
/// Keeps the [`SAMPLE_SIZE`] chunks with the lexicographically smallest
/// transformed addresses, returned in ascending transformed-address order, via a
/// sorted insertion that drops the largest element once full.
///
/// On a transformed-address tie the content-addressed chunk wins the slot, so the
/// on-chain ordering check cannot be gamed by a SOC colliding with a CAC.
///
/// Selection and the committed bytes are insertion-order independent. Only the
/// kept stamp is order-dependent: the same content under several batches ties and
/// collapses to one slot keeping the last CAC seen. This is consensus-safe, since
/// the commitment hashes `chunk_address || transformed_address`
/// ([`reserve_commitment_content`]) which is identical across the tied entries and
/// the contract accepts any owning batch's stamp.
#[must_use]
pub fn reserve_sample(candidates: impl IntoIterator<Item = SampleItem>) -> Vec<SampleItem> {
    let mut sample: Vec<SampleItem> = Vec::with_capacity(SAMPLE_SIZE + 1);

    for item in candidates {
        insert_sample_item(&mut sample, item);
    }

    sample
}

fn insert_sample_item(sample: &mut Vec<SampleItem>, item: SampleItem) {
    let key = item.transformed_address;

    // First slot not strictly smaller than `key`: a tie or the insertion point.
    let Some(pos) = sample
        .iter()
        .position(|s| s.transformed_address.as_slice() >= key.as_slice())
    else {
        // Larger than every incumbent: append only while not yet full.
        if sample.len() < SAMPLE_SIZE {
            sample.push(item);
        }
        return;
    };

    match sample.get_mut(pos) {
        // Tie: a CAC replaces the incumbent, a SOC does not, so a CAC always
        // wins the slot. No new slot is consumed either way.
        Some(incumbent) if incumbent.transformed_address == key => {
            if item.chunk.is_content() {
                *incumbent = item;
            }
        }
        _ => {
            sample.insert(pos, item);
            if sample.len() > SAMPLE_SIZE {
                sample.truncate(SAMPLE_SIZE);
            }
        }
    }
}

/// Build the reserve-commitment chunk body: each item's `chunk_address ||
/// transformed_address` concatenated, `SAMPLE_SIZE * 64` bytes. Returns the body
/// only; callers BMT-hash it with span `64 * SAMPLE_SIZE`.
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

    fn cac_item(anchor: SampleAnchor, payload: &[u8]) -> SampleItem {
        let chunk = DefaultContentChunk::new(payload.to_vec()).unwrap();
        SampleItem::new(anchor, chunk.into())
    }

    fn sample_anchor() -> SampleAnchor {
        SampleAnchor::new(B256::from_slice(b"swarm-test-anchor-deterministic!"))
    }

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
        // A CAC and a SOC sharing a transformed address; the CAC must win.
        let base = cac_item(anchor, &[7; 16]);
        let soc_dup = SampleItem {
            chunk: soc_chunk(),
            ..base.clone()
        };

        // SOC first, then CAC.
        let out = reserve_sample(vec![soc_dup.clone(), base.clone()]);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].chunk.is_content(),
            "CAC must replace SOC on a transformed tie"
        );

        // CAC first, then SOC.
        let out = reserve_sample(vec![base.clone(), soc_dup]);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].chunk.is_content(),
            "incumbent CAC must survive a SOC tie"
        );
    }
}
