//! Consensus spec tests for the per-entry reserve.
//!
//! Each test builds a `DbReserve` over an in-memory redb `Database` and a
//! `DbBatchStore` with a real `Batch`, signing real stamps so the
//! validate-on-ingest path runs as in production. Covers the
//! consensus-load-bearing invariants: per-entry size counting, newest-wins
//! arbitration on the full `(batchID, stampIndex)` slot, refcounted payload
//! survival under partial eviction, exact-stamp-in-proof, and full compaction
//! (no tombstones) on removal.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions over known-bounds fixtures"
)]

use super::*;
use alloy_primitives::Address;
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use nectar_postage::{Batch, PostageContext, StampDigest, StampIndex};
use nectar_primitives::{Bin, Chunk, DefaultContentChunk as ContentChunk};
use std::sync::Arc;
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_api::SwarmIdentity as _;
use vertex_swarm_postage::DbBatchStore;
use vertex_swarm_test_utils::MockIdentity;

const THRESHOLD: u64 = 8;
// bucket_depth 1 splits by the top bit, so distinct addresses can be coerced
// into the same bucket (top bit 0) to compete for one arbiter slot.
const BUCKET_DEPTH: u8 = 1;
const DEPTH: u8 = 18;

fn signer() -> PrivateKeySigner {
    PrivateKeySigner::from_bytes(&B256::repeat_byte(0x42)).expect("valid signer")
}

/// Ample value so it is not expired against a zero cumulative payout.
fn batch_for(owner: Address, id: B256) -> Batch {
    Batch::new(id, 1_000_000, 0, owner, DEPTH, BUCKET_DEPTH, false)
}

/// Past the confirmation threshold with zero payout, so a fresh batch is usable.
fn live_context() -> PostageContext {
    PostageContext::new(THRESHOLD + 1, 0)
}

/// A content chunk whose address falls in bucket 0 (top bit clear), found by
/// varying the payload.
fn content_chunk_in_bucket0(seed: u64) -> (nectar_primitives::AnyChunk, ChunkAddress) {
    for n in 0..100_000u64 {
        let payload = format!("vertex reserve consensus fixture {seed}/{n}").into_bytes();
        let chunk = ContentChunk::new(payload).expect("valid content chunk");
        let addr = *chunk.address();
        if addr.as_slice()[0] & 0x80 == 0 {
            return (chunk.into(), addr);
        }
    }
    panic!("no bucket-0 content chunk found within the search bound");
}

/// A content chunk whose address has exactly `target_po` leading bits clear, so
/// its proximity order to the zero overlay is `target_po`. Lets a test place
/// chunks at chosen distances to assert furthest-first eviction.
fn content_chunk_at_po(seed: u64, target_po: u8) -> (nectar_primitives::AnyChunk, ChunkAddress) {
    let overlay = ChunkAddress::with_first_byte(0x00);
    for n in 0..1_000_000u64 {
        let payload = format!("vertex reserve po fixture {seed}/{target_po}/{n}").into_bytes();
        let chunk = ContentChunk::new(payload).expect("valid content chunk");
        let addr = *chunk.address();
        if addr.proximity(&overlay).get() == target_po {
            return (chunk.into(), addr);
        }
    }
    panic!("no content chunk at proximity order {target_po} within the search bound");
}

/// Bucket is derived from the address so `validate_bucket` passes.
fn signed_stamp(
    signer: &PrivateKeySigner,
    batch: &Batch,
    address: &ChunkAddress,
    index: u32,
    timestamp: u64,
) -> Stamp {
    let bucket = batch.bucket_for_address(address);
    let stamp_index = StampIndex::new(bucket, index);
    let digest = StampDigest::new(*address, batch.id(), stamp_index, timestamp);
    let sig = signer
        .sign_hash_sync(&alloy_primitives::eip191_hash_message(
            digest.to_prehash().as_slice(),
        ))
        .expect("sign");
    Stamp::with_index(batch.id(), stamp_index, timestamp, sig)
}

/// A test reserve and its shared database. The reserve owns its own
/// `DbBatchStore`; the fixture holds a second store over the same database for
/// populating and reading batches, so both see identical persisted state.
struct Fixture {
    reserve: DbReserve<RedbDatabase, DbBatchStore<RedbDatabase>>,
    db: Arc<RedbDatabase>,
    batches: DbBatchStore<RedbDatabase>,
    overlay: OverlayAddress,
    signer: PrivateKeySigner,
}

impl Fixture {
    fn new() -> Self {
        Self::with_batches(&[B256::repeat_byte(0x11)])
    }

    /// All batches are owned by the shared signer, with the live context persisted.
    fn with_batches(batch_ids: &[B256]) -> Self {
        Self::with_capacity(batch_ids, 10_000)
    }

    /// As [`with_batches`](Self::with_batches), at a chosen reserve capacity so the
    /// furthest-eviction trigger can be exercised at a small bound.
    fn with_capacity(batch_ids: &[B256], capacity: u64) -> Self {
        let db = RedbDatabase::in_memory().unwrap().into_arc();
        let batches = DbBatchStore::new(Arc::clone(&db)).unwrap();
        let signer = signer();
        let owner = signer.address();
        for id in batch_ids {
            batches.put(batch_for(owner, *id)).unwrap();
        }
        batches.set_context(live_context()).unwrap();

        let identity = MockIdentity::with_first_byte(0x00);
        let overlay = identity.overlay_address();
        let reserve_batches = DbBatchStore::new(Arc::clone(&db)).unwrap();
        let reserve = DbReserve::new(
            Arc::clone(&db),
            &identity,
            reserve_batches,
            AdmissionValidator::new(THRESHOLD),
            capacity,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        )
        .unwrap();
        Self {
            reserve,
            db,
            batches,
            overlay,
            signer,
        }
    }

    fn batch_id(&self) -> BatchId {
        BatchId::repeat_byte(0x11)
    }

    /// Stamp `chunk` under `batch_id` at `(index, timestamp)` and put it.
    fn put(
        &self,
        chunk: &nectar_primitives::AnyChunk,
        addr: &ChunkAddress,
        batch_id: BatchId,
        index: u32,
        timestamp: u64,
    ) -> SwarmResult<()> {
        let batch = self.batches.get(&batch_id).unwrap().expect("batch present");
        let stamp = signed_stamp(&self.signer, &batch, addr, index, timestamp);
        self.reserve
            .put(CachedChunk::new(chunk.clone(), Some(stamp)))
    }

    fn row_count<T: Table>(&self) -> u64 {
        self.db.view(|tx| tx.count::<T>()).unwrap() as u64
    }

    fn payload_refcnt(&self, addr: &ChunkAddress) -> Option<u64> {
        self.db
            .view(|tx| Ok(tx.get::<Payload>(*addr)?.map(|p| p.refcnt)))
            .unwrap()
    }
}

#[test]
fn reserve_size_counts_stamped_entries_not_addresses() {
    // One content address under N batches leaves size == N (one Entry per
    // (batchID, stampIndex, address)), not 1. Size feeds the consensus-committed
    // storage_radius.
    let ids = [
        B256::repeat_byte(0x11),
        B256::repeat_byte(0x22),
        B256::repeat_byte(0x33),
    ];
    let fx = Fixture::with_batches(&ids);
    let (chunk, addr) = content_chunk_in_bucket0(1);

    for (i, id) in ids.iter().enumerate() {
        fx.put(&chunk, &addr, *id, 0, 100 + i as u64).unwrap();
    }

    assert_eq!(
        fx.reserve.count().unwrap(),
        ids.len() as u64,
        "size counts distinct stamped entries, not the single content address"
    );
    assert_eq!(
        fx.row_count::<Entry>(),
        ids.len() as u64,
        "one Entry row per stamped entry"
    );
    assert_eq!(fx.row_count::<Payload>(), 1, "one shared content payload");
    assert_eq!(fx.payload_refcnt(&addr), Some(ids.len() as u64));
}

#[test]
fn newest_timestamp_wins_full_index_keying() {
    // On one (batchID, stampIndex) slot: a newer timestamp displaces the entry,
    // an equal or older timestamp rejects (prev >= curr). Distinct indices in
    // the same bucket are separate slots and both admit.
    let fx = Fixture::new();
    let id = fx.batch_id();
    let (chunk, addr) = content_chunk_in_bucket0(2);

    fx.put(&chunk, &addr, id, 0, 100).unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 1);

    // Newer timestamp: restamp, size unchanged, newer stamp survives.
    fx.put(&chunk, &addr, id, 0, 200).unwrap();
    assert_eq!(
        fx.reserve.count().unwrap(),
        1,
        "restamp displaces the old entry; size unchanged"
    );
    let got = fx.reserve.get(&addr).unwrap().expect("present");
    assert_eq!(got.stamp().expect("stamped").timestamp(), 200);

    // Equal timestamp: reject.
    fx.put(&chunk, &addr, id, 0, 200).unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 1);
    assert_eq!(
        fx.reserve
            .get(&addr)
            .unwrap()
            .unwrap()
            .stamp()
            .unwrap()
            .timestamp(),
        200,
        "equal-timestamp re-presentation does not overwrite"
    );

    // Older timestamp: reject.
    fx.put(&chunk, &addr, id, 0, 150).unwrap();
    assert_eq!(
        fx.reserve
            .get(&addr)
            .unwrap()
            .unwrap()
            .stamp()
            .unwrap()
            .timestamp(),
        200,
        "older stamp is stale and rejected"
    );

    // A distinct index in the same bucket is a different slot: it admits.
    fx.put(&chunk, &addr, id, 1, 50).unwrap();
    assert_eq!(
        fx.reserve.count().unwrap(),
        2,
        "distinct stampIndex in the same bucket is a separate slot"
    );
}

#[test]
fn refcounted_payload_survives_partial_eviction() {
    // Same content under two batches: one Payload row at refcnt 2; the second
    // put bumps the refcount without rewriting the body. Evicting one entry
    // leaves the body (refcnt 1); evicting the last drops it.
    let ids = [B256::repeat_byte(0x11), B256::repeat_byte(0x22)];
    let fx = Fixture::with_batches(&ids);
    let (chunk, addr) = content_chunk_in_bucket0(3);

    fx.put(&chunk, &addr, ids[0], 0, 100).unwrap();
    let body_after_first = fx
        .db
        .view(|tx| Ok(tx.get::<Payload>(addr)?.map(|p| p.typed_bytes)))
        .unwrap()
        .expect("payload present");

    fx.put(&chunk, &addr, ids[1], 0, 100).unwrap();
    assert_eq!(fx.row_count::<Payload>(), 1, "one shared payload");
    assert_eq!(
        fx.payload_refcnt(&addr),
        Some(2),
        "second batch bumps refcnt"
    );
    let body_after_second = fx
        .db
        .view(|tx| Ok(tx.get::<Payload>(addr)?.map(|p| p.typed_bytes)))
        .unwrap()
        .expect("payload present");
    assert_eq!(
        body_after_first, body_after_second,
        "second batch must not rewrite the shared body"
    );
    assert_eq!(fx.reserve.count().unwrap(), 2, "two stamped entries");

    let evicted = fx.reserve.evict_furthest().unwrap();
    assert_eq!(evicted, Some(addr));
    assert_eq!(fx.reserve.count().unwrap(), 1, "one entry removed");
    assert_eq!(
        fx.payload_refcnt(&addr),
        Some(1),
        "shared body survives partial eviction"
    );
    let got = fx.reserve.get(&addr).unwrap().expect("survivor present");
    assert_eq!(got.address(), &addr);

    fx.reserve.evict_furthest().unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 0);
    assert_eq!(fx.payload_refcnt(&addr), None, "last entry drops the body");
}

#[test]
fn restamp_to_different_address_cleans_displaced_rows() {
    // Restamping the (batchID, stampIndex) slot onto a different address must
    // delete the displaced entry's rows using the displaced address's proximity
    // and decrement its payload refcount exactly once (no orphaned rows, no
    // leaked refcount).
    let fx = Fixture::new();
    let id = fx.batch_id();
    // Both in bucket 0, so they share index slot 0.
    let (chunk_a, addr_a) = content_chunk_in_bucket0(10);
    let (chunk_b, addr_b) = content_chunk_in_bucket0(20);
    assert_ne!(addr_a, addr_b, "fixtures must be distinct addresses");

    fx.put(&chunk_a, &addr_a, id, 0, 100).unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 1);
    assert!(fx.reserve.contains(&addr_a));

    // Restamp the slot onto B at a newer timestamp: A displaced, B admitted.
    fx.put(&chunk_b, &addr_b, id, 0, 200).unwrap();
    assert_eq!(
        fx.reserve.count().unwrap(),
        1,
        "restamp to a different address displaces A and admits B"
    );

    assert!(!fx.reserve.contains(&addr_a), "displaced A body removed");
    assert_eq!(fx.payload_refcnt(&addr_a), None, "A refcount not leaked");
    assert!(fx.reserve.contains(&addr_b), "B present");
    assert_eq!(fx.payload_refcnt(&addr_b), Some(1));

    // Exactly one of each row remains (B's), no A residue.
    assert_eq!(fx.row_count::<Entry>(), 1, "one Entry row (B)");
    assert_eq!(fx.row_count::<BatchGroup>(), 1, "one BatchGroup row (B)");
    assert_eq!(fx.row_count::<Replay>(), 1, "one Replay row (B)");
    assert_eq!(fx.row_count::<Payload>(), 1, "one Payload row (B)");
}

#[test]
fn get_returns_exact_admitting_stamp() {
    // get() surfaces the per-entry admitting stamp byte-for-byte, not one
    // re-loaded by batchID alone. An inclusion proof carries exactly this stamp.
    let fx = Fixture::new();
    let id = fx.batch_id();
    let (chunk, addr) = content_chunk_in_bucket0(4);

    let batch = fx.batches.get(&id).unwrap().unwrap();
    let stamp = signed_stamp(&fx.signer, &batch, &addr, 0, 12_345);
    let expected_bytes = stamp.to_bytes();
    fx.reserve
        .put(CachedChunk::new(chunk.clone(), Some(stamp.clone())))
        .unwrap();

    let got = fx.reserve.get(&addr).unwrap().expect("present");
    let got_stamp = got.stamp().expect("stamped");
    assert_eq!(got_stamp.timestamp(), 12_345);
    assert_eq!(
        got_stamp.to_bytes(),
        expected_bytes,
        "the exact admitting stamp bytes are surfaced"
    );
    assert_eq!(got.chunk(), &chunk);
}

#[test]
fn removal_fully_compacts_all_tables_no_tombstones() {
    // remove() deletes every row of every stamped entry for an address across
    // all six tables, leaving no tombstone.
    let ids = [B256::repeat_byte(0x11), B256::repeat_byte(0x22)];
    let fx = Fixture::with_batches(&ids);
    let (chunk, addr) = content_chunk_in_bucket0(5);

    fx.put(&chunk, &addr, ids[0], 0, 100).unwrap();
    fx.put(&chunk, &addr, ids[1], 0, 100).unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 2);

    fx.reserve.remove(&addr).unwrap();

    assert_eq!(fx.reserve.count().unwrap(), 0);
    assert!(!fx.reserve.contains(&addr));
    assert_eq!(fx.row_count::<Entry>(), 0, "no Entry tombstones");
    assert_eq!(fx.row_count::<BatchGroup>(), 0, "no BatchGroup tombstones");
    assert_eq!(fx.row_count::<Replay>(), 0, "no Replay tombstones");
    assert_eq!(fx.row_count::<Payload>(), 0, "no Payload tombstones");
    // The arbiter slot is cleared, so it does not pin a stale newest timestamp.
    let slot0 = StampSlotKey::new(BatchId::from(ids[0]), StampIndex::new(0, 0));
    assert!(
        fx.db
            .view(|tx| tx.get::<StampIndexTable>(slot0))
            .unwrap()
            .is_none(),
        "arbiter slot cleared on full removal"
    );
}

#[test]
fn unknown_batch_and_stampless_puts_are_rejected() {
    // A stamp for an unknown batch and a stampless put are both refused, writing
    // nothing.
    let fx = Fixture::new();
    let (chunk, addr) = content_chunk_in_bucket0(6);

    // Stampless.
    let err = fx
        .reserve
        .put(CachedChunk::new(chunk.clone(), None))
        .unwrap_err();
    assert!(matches!(err, SwarmError::InvalidChunk { .. }));
    assert!(!fx.reserve.contains(&addr));

    // Sign under a batch id the store does not hold.
    let unknown = Batch::new(
        B256::repeat_byte(0x99),
        1_000_000,
        0,
        fx.signer.address(),
        DEPTH,
        BUCKET_DEPTH,
        false,
    );
    let stamp = signed_stamp(&fx.signer, &unknown, &addr, 0, 100);
    let err = fx
        .reserve
        .put(CachedChunk::new(chunk, Some(stamp)))
        .unwrap_err();
    assert!(matches!(err, SwarmError::InvalidChunk { .. }));
    assert_eq!(fx.reserve.count().unwrap(), 0);
}

#[test]
fn bin_scan_replays_entries_in_insertion_order() {
    // The Replay table surfaces each entry's (address, batch, stampHash) in
    // per-bin insertion order without a body read.
    let ids = [B256::repeat_byte(0x11), B256::repeat_byte(0x22)];
    let fx = Fixture::with_batches(&ids);
    let (chunk, addr) = content_chunk_in_bucket0(7);
    let bin = addr.bin(&fx.overlay);

    fx.put(&chunk, &addr, ids[0], 0, 100).unwrap();
    fx.put(&chunk, &addr, ids[1], 0, 100).unwrap();

    let items: Vec<_> = fx
        .reserve
        .scan_bin_from(bin, 0)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(items.len(), 2, "two entries replayed");
    assert!(
        items[0].seq < items[1].seq,
        "replayed ascending by insertion sequence"
    );
    assert!(items.iter().all(|i| i.address == addr));
}

// Sample-at-most-once collapse is a sampler property; the reserve only supplies
// the per-entry Replay projection, covered by
// bin_scan_replays_entries_in_insertion_order.
#[test]
#[ignore = "sampler property; reserve only supplies the per-entry Replay projection"]
fn sample_collapses_duplicate_transformed_address() {}

// --- radius dynamics: expiry -> evict_batch end to end -----------

#[test]
fn batch_expiry_sweep_evicts_only_the_expired_batch() {
    use crate::expiry::ExpirySweep;

    // Two batches with distinct content addresses, so neither eviction touches
    // the other's refcounted payload.
    let live_id = B256::repeat_byte(0x11);
    let expiring_id = B256::repeat_byte(0x22);
    let fx = Fixture::with_batches(&[live_id, expiring_id]);
    let (chunk_a, addr_a) = content_chunk_in_bucket0(101);
    let (chunk_b, addr_b) = content_chunk_in_bucket0(202);

    fx.put(&chunk_a, &addr_a, live_id, 0, 100).unwrap();
    fx.put(&chunk_b, &addr_b, expiring_id, 0, 100).unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 2, "two stamped entries");

    // Lower the expiring batch's value below the cumulative payout, leaving the
    // live one above, so only the expiring batch is expired.
    let mut expiring = fx.batches.get(&expiring_id).unwrap().unwrap();
    expiring.set_value(500);
    fx.batches.put(expiring).unwrap();
    // total_amount 1000 >= expiring value (500), < live value (1_000_000).
    fx.batches
        .set_context(PostageContext::new(THRESHOLD + 1, 1000))
        .unwrap();

    let reserve_batches = DbBatchStore::new(Arc::clone(&fx.db)).unwrap();
    let report = ExpirySweep::new(&reserve_batches, &fx.reserve)
        .run()
        .unwrap();

    assert_eq!(
        report.evicted_batches,
        vec![expiring_id],
        "only the expired batch is swept"
    );
    assert_eq!(report.evicted_entries, 1, "its single entry is removed");
    assert_eq!(
        fx.reserve.count().unwrap(),
        1,
        "the live batch's entry survives expiry of the other"
    );
    assert_eq!(fx.payload_refcnt(&addr_b), None, "expired payload removed");
    assert_eq!(
        fx.payload_refcnt(&addr_a),
        Some(1),
        "live payload still refcounted"
    );

    // Idempotent: a second sweep at the same context evicts nothing.
    let again = ExpirySweep::new(&reserve_batches, &fx.reserve)
        .run()
        .unwrap();
    assert!(again.evicted_batches.is_empty(), "sweep is idempotent");
    assert_eq!(fx.reserve.count().unwrap(), 1);
}

#[test]
fn settable_radius_cell_round_trips_through_the_read_seam() {
    // A write via SettableRadius::set_storage_radius is observed by the
    // storage_radius / is_responsible_for reads.
    use vertex_swarm_api::SettableRadius;

    let fx = Fixture::new();
    assert_eq!(
        fx.reserve.storage_radius(),
        StorageRadius::ZERO,
        "constructed at the configured radius"
    );

    let target = StorageRadius::new(Bin::try_from(5).unwrap());
    // Through the trait object form to prove object safety.
    let dyn_reserve: &dyn SettableRadius = &fx.reserve;
    dyn_reserve.set_storage_radius(target);

    assert_eq!(
        fx.reserve.storage_radius(),
        target,
        "the read observes the committed radius"
    );
    // is_responsible_for evaluates against the new radius.
    let (_chunk, addr) = content_chunk_in_bucket0(909);
    let po = addr.proximity(&fx.overlay).get();
    assert_eq!(
        fx.reserve.is_responsible_for(&addr),
        po >= 5,
        "responsibility is evaluated against the committed radius"
    );
}

#[test]
fn radius_controller_apply_commits_a_shrink_through_the_seam() {
    // From a radius above the floor, an under-filled idle reserve shrinks one
    // step, and apply commits the shallower radius through SettableRadius.
    use crate::RadiusController;

    let fx = Fixture::new();
    // Above the floor with no entries (within-radius 0 < threshold).
    fx.reserve
        .set_storage_radius(StorageRadius::new(Bin::try_from(5).unwrap()));

    let controller = RadiusController::new(StorageRadius::ZERO);
    let committed = controller
        .apply(&fx.reserve, /* syncing_idle */ true)
        .unwrap();

    assert_eq!(
        committed,
        StorageRadius::new(Bin::try_from(4).unwrap()),
        "an under-filled idle reserve shrinks one step"
    );
    assert_eq!(
        fx.reserve.storage_radius(),
        committed,
        "apply commits the derived radius through the settable cell"
    );
}

#[test]
fn radius_controller_apply_holds_at_floor() {
    // At the configured floor the shrink rule does not fire, so apply commits
    // nothing and reports the unchanged radius.
    use crate::RadiusController;

    let fx = Fixture::new();
    let controller = RadiusController::new(StorageRadius::ZERO);
    let committed = controller.apply(&fx.reserve, true).unwrap();
    assert_eq!(committed, StorageRadius::ZERO, "held at the floor");
    assert_eq!(fx.reserve.storage_radius(), StorageRadius::ZERO);
}

#[test]
fn nothing_evicted_when_no_batch_is_expired() {
    use crate::expiry::ExpirySweep;

    // Both batches retain ample value against a zero cumulative payout.
    let ids = [B256::repeat_byte(0x11), B256::repeat_byte(0x22)];
    let fx = Fixture::with_batches(&ids);
    let (chunk, addr) = content_chunk_in_bucket0(303);
    fx.put(&chunk, &addr, ids[0], 0, 100).unwrap();
    fx.put(&chunk, &addr, ids[1], 0, 100).unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 2);

    let reserve_batches = DbBatchStore::new(Arc::clone(&fx.db)).unwrap();
    let report = ExpirySweep::new(&reserve_batches, &fx.reserve)
        .run()
        .unwrap();
    assert!(report.evicted_batches.is_empty(), "no batch expired");
    assert_eq!(report.evicted_entries, 0);
    assert_eq!(fx.reserve.count().unwrap(), 2, "nothing evicted");
}

#[test]
fn expired_event_evicts_reserve_before_store_removal() {
    // On an `Expired` event the reserve entries are shed before the batch leaves
    // the store; otherwise the reconciliation sweep (which reads batch_ids)
    // could never see them and they would be orphaned, inflating size and radius.
    use crate::expiry::ExpirySweep;
    use std::cell::Cell;

    let expiring_id = B256::repeat_byte(0x33);
    let fx = Fixture::with_batches(&[expiring_id]);
    let (chunk, addr) = content_chunk_in_bucket0(404);
    fx.put(&chunk, &addr, expiring_id, 0, 100).unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 1, "one stamped entry");

    let reserve_batches = DbBatchStore::new(Arc::clone(&fx.db)).unwrap();
    let sweep = ExpirySweep::new(&reserve_batches, &fx.reserve);

    // The acknowledgement (store removal) asserts the reserve is already empty,
    // proving the ordering.
    let acked = Cell::new(false);
    let removed = sweep
        .on_expired_event::<_, std::io::Error>(expiring_id, || {
            assert_eq!(
                fx.reserve.count().unwrap(),
                0,
                "entries must be evicted before the store removal runs"
            );
            acked.set(true);
            Ok(())
        })
        .unwrap();

    assert_eq!(removed, 1, "the batch's single entry is evicted");
    assert!(acked.get(), "the acknowledgement ran after eviction");
    assert_eq!(fx.reserve.count().unwrap(), 0, "reserve drained");
    assert_eq!(fx.payload_refcnt(&addr), None, "payload removed");
}

#[test]
fn expired_event_does_not_acknowledge_when_eviction_fails_is_skipped() {
    // A failing acknowledgement leaves the eviction done but surfaces the error
    // so the caller can retry the removal.
    use crate::expiry::ExpirySweep;

    let expiring_id = B256::repeat_byte(0x44);
    let fx = Fixture::with_batches(&[expiring_id]);
    let (chunk, addr) = content_chunk_in_bucket0(505);
    fx.put(&chunk, &addr, expiring_id, 0, 100).unwrap();

    let reserve_batches = DbBatchStore::new(Arc::clone(&fx.db)).unwrap();
    let sweep = ExpirySweep::new(&reserve_batches, &fx.reserve);

    let err = sweep
        .on_expired_event::<_, std::io::Error>(expiring_id, || {
            Err(std::io::Error::other("store removal failed"))
        })
        .unwrap_err();
    assert!(
        format!("{err}").contains("store removal failed"),
        "acknowledge error is funnelled through SwarmError::storage"
    );
    // The eviction still happened (it precedes the acknowledge); the caller
    // retries the store removal, which a later `run` backstop also catches.
    assert_eq!(fx.reserve.count().unwrap(), 0, "eviction already applied");
    assert_eq!(fx.payload_refcnt(&addr), None);
}

// --- furthest-eviction to capacity -----------------------------------

#[test]
fn evict_to_capacity_drops_the_furthest_entries_first() {
    // Capacity 2, three entries at distinct proximity orders. The two furthest
    // (lowest proximity) are evicted; the closest survives.
    let id = B256::repeat_byte(0x11);
    let fx = Fixture::with_capacity(&[id], 2);
    let batch_id = fx.batch_id();

    let (far_chunk, far_addr) = content_chunk_at_po(1, 0); // furthest
    let (mid_chunk, mid_addr) = content_chunk_at_po(2, 3); // middle
    let (near_chunk, near_addr) = content_chunk_at_po(3, 9); // closest

    // Distinct stamp indices so the per-bucket arbiter slots never collide.
    fx.put(&far_chunk, &far_addr, batch_id, 0, 100).unwrap();
    fx.put(&mid_chunk, &mid_addr, batch_id, 1, 100).unwrap();
    fx.put(&near_chunk, &near_addr, batch_id, 2, 100).unwrap();
    assert_eq!(
        fx.reserve.count().unwrap(),
        3,
        "three entries, over capacity"
    );

    let removed = fx.reserve.evict_to_capacity().unwrap();
    assert_eq!(removed, 1, "one entry over capacity is shed");
    assert_eq!(fx.reserve.count().unwrap(), 2, "back within capacity");

    assert!(
        !fx.reserve.contains(&far_addr),
        "the furthest entry is evicted"
    );
    assert!(
        fx.reserve.contains(&mid_addr),
        "the middle entry is retained"
    );
    assert!(
        fx.reserve.contains(&near_addr),
        "the closest entry is retained"
    );

    // Idempotent at capacity.
    assert_eq!(
        fx.reserve.evict_to_capacity().unwrap(),
        0,
        "no eviction when already within capacity"
    );
}

#[test]
fn evict_to_capacity_does_not_rewind_bin_cursors() {
    // A bin's monotonic insertion cursor must survive eviction so a pull-sync
    // resume point stays valid: a scan from a sequence past the evicted entry
    // still resolves the survivor.
    let id = B256::repeat_byte(0x11);
    let fx = Fixture::with_capacity(&[id], 1);
    let batch_id = fx.batch_id();

    // Two entries in the same bin (same proximity order to the zero overlay), so
    // their insertion sequences are 1 and 2 in that bin.
    let (first_chunk, first_addr) = content_chunk_at_po(1, 5);
    let (second_chunk, second_addr) = content_chunk_at_po(2, 5);
    let bin = first_addr.bin(&fx.overlay);
    assert_eq!(bin, second_addr.bin(&fx.overlay), "fixtures share a bin");

    fx.put(&first_chunk, &first_addr, batch_id, 0, 100).unwrap();
    fx.put(&second_chunk, &second_addr, batch_id, 1, 100)
        .unwrap();
    let cursor_before = fx.reserve.bin_cursor(bin).unwrap();
    assert_eq!(
        cursor_before, 2,
        "two insertions advance the bin cursor to 2"
    );

    // Over capacity by one: the furthest (here the first by tie order) is shed.
    let removed = fx.reserve.evict_to_capacity().unwrap();
    assert_eq!(removed, 1);

    let cursor_after = fx.reserve.bin_cursor(bin).unwrap();
    assert_eq!(
        cursor_after, cursor_before,
        "the monotonic bin cursor is never rewound on eviction"
    );

    // The evicted entry's Replay row is compacted away, leaving exactly the
    // survivor; its sequence is one of the two originally assigned (1 or 2),
    // never renumbered, so a resume point computed before eviction stays valid.
    let items: Vec<_> = fx
        .reserve
        .scan_bin_from(bin, 0)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(items.len(), 1, "exactly the survivor's Replay row remains");
    assert!(
        items[0].seq == 1 || items[0].seq == 2,
        "the survivor keeps its original sequence; no renumbering"
    );
    // Resuming from one past the cursor yields nothing (no new inserts), proving
    // the cursor was not rewound below the survivor.
    let tail: Vec<_> = fx
        .reserve
        .scan_bin_from(bin, cursor_after + 1)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(tail.is_empty(), "no entries beyond the unchanged cursor");
}

#[test]
fn evict_to_capacity_frees_the_body_only_on_the_last_reference() {
    // One content address under two batches: the shared payload survives the
    // first entry's eviction (refcnt 2 -> 1) and is freed only when the last
    // referencing entry goes.
    let ids = [B256::repeat_byte(0x11), B256::repeat_byte(0x22)];
    let fx = Fixture::with_capacity(&ids, 1);
    let (chunk, addr) = content_chunk_at_po(1, 4);

    fx.put(&chunk, &addr, ids[0], 0, 100).unwrap();
    fx.put(&chunk, &addr, ids[1], 0, 100).unwrap();
    assert_eq!(
        fx.reserve.count().unwrap(),
        2,
        "two stamped entries share a body"
    );
    assert_eq!(fx.payload_refcnt(&addr), Some(2));

    // Capacity 1, so one of the two entries is shed; the body remains refcounted.
    let removed = fx.reserve.evict_to_capacity().unwrap();
    assert_eq!(removed, 1, "one entry over capacity is shed");
    assert_eq!(fx.reserve.count().unwrap(), 1);
    assert_eq!(
        fx.payload_refcnt(&addr),
        Some(1),
        "shared body survives while another entry references it"
    );
    assert!(fx.reserve.contains(&addr), "body still present");

    // Drop the reserve to capacity 0 by evicting the last entry directly; the
    // body is freed only now.
    fx.reserve.evict_furthest().unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 0);
    assert_eq!(
        fx.payload_refcnt(&addr),
        None,
        "body freed only when the last referencing entry is evicted"
    );
}
