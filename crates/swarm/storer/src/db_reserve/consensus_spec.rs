//! Consensus spec tests for the per-entry reserve.
//!
//! Each test builds a `DbReserve` over an in-memory redb-backed `vertex-storage`
//! `Database`, a `DbBatchStore` populated with a real `Batch`, and signs real
//! stamps with an `alloy-signer-local` wallet so the validate-on-ingest admission
//! path runs exactly as in production. The invariants asserted here are the
//! consensus-load-bearing ones: per-entry size counting, newest-wins / equal- and
//! older-reject arbitration on the full `(batchID, stampIndex)` slot, refcounted
//! payload survival under partial eviction, exact-stamp-in-proof, and full
//! compaction (no tombstones) on removal.

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
use nectar_primitives::{Chunk, DefaultContentChunk as ContentChunk};
use std::sync::Arc;
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_api::SwarmIdentity as _;
use vertex_swarm_postage::DbBatchStore;
use vertex_swarm_test_utils::MockIdentity;

const THRESHOLD: u64 = 8;
// bucket_depth 1 splits the address space by the top bit, so two distinct
// content addresses can be coerced into the *same* bucket (top bit 0) and
// therefore compete for the same `(batch, stampIndex)` arbiter slot. depth 18
// gives ample per-bucket capacity for index 0.
const BUCKET_DEPTH: u8 = 1;
const DEPTH: u8 = 18;

fn signer() -> PrivateKeySigner {
    PrivateKeySigner::from_bytes(&B256::repeat_byte(0x42)).expect("valid signer")
}

/// A batch owned by `owner`, created at block 0, with ample value so it is
/// not expired against a zero cumulative payout.
fn batch_for(owner: Address, id: B256) -> Batch {
    Batch::new(id, 1_000_000, 0, owner, DEPTH, BUCKET_DEPTH, false)
}

/// A live context past the confirmation threshold with zero payout (so a
/// fresh batch is usable and not expired).
fn live_context() -> PostageContext {
    PostageContext::new(THRESHOLD + 1, 0)
}

/// A content chunk whose address falls in bucket 0 of a `BUCKET_DEPTH` batch
/// (top bit clear), searched by varying the payload. Returns the chunk and
/// its address.
fn content_chunk_in_bucket0(seed: u64) -> (nectar_primitives::AnyChunk, ChunkAddress) {
    for n in 0..100_000u64 {
        let payload = format!("vertex reserve consensus fixture {seed}/{n}").into_bytes();
        let chunk = ContentChunk::new(payload).expect("valid content chunk");
        let addr = *chunk.address();
        // bucket_for_address with bucket_depth 1 == top bit of byte 0.
        if addr.as_slice()[0] & 0x80 == 0 {
            return (chunk.into(), addr);
        }
    }
    panic!("no bucket-0 content chunk found within the search bound");
}

/// Sign a real stamp for `address` under `batch` at `timestamp`, at the given
/// within-bucket `index`, with the bucket derived from the address so
/// `validate_bucket` passes.
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

/// A test reserve and its shared database, plus the batch store the
/// validate-on-ingest path reads.
///
/// The reserve owns its own `DbBatchStore` (the `BatchStore` trait is
/// implemented on the store, not on `Arc<store>`), and the fixture holds a
/// second store over the *same* database for populating and reading batches:
/// both see identical persisted state.
struct Fixture {
    reserve: DbReserve<RedbDatabase, DbBatchStore<RedbDatabase>>,
    db: Arc<RedbDatabase>,
    batches: DbBatchStore<RedbDatabase>,
    overlay: OverlayAddress,
    signer: PrivateKeySigner,
}

impl Fixture {
    /// Build a reserve over a fresh in-memory database with a single batch
    /// already registered and the live context persisted.
    fn new() -> Self {
        Self::with_batches(&[B256::repeat_byte(0x11)])
    }

    /// Build a reserve whose batch store already holds the given batch ids
    /// (all owned by the shared signer), with the live context persisted.
    fn with_batches(batch_ids: &[B256]) -> Self {
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
        // The reserve's own store handle over the same shared database.
        let reserve_batches = DbBatchStore::new(Arc::clone(&db)).unwrap();
        let reserve = DbReserve::new(
            Arc::clone(&db),
            &identity,
            reserve_batches,
            AdmissionValidator::new(THRESHOLD),
            10_000,
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

    /// The single batch id this fixture was built with.
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

    /// Count rows in a table via a full cursor walk.
    fn row_count<T: Table>(&self) -> u64 {
        self.db.view(|tx| tx.count::<T>()).unwrap() as u64
    }

    /// The refcount stored for an address's payload, or `None` if absent.
    fn payload_refcnt(&self, addr: &ChunkAddress) -> Option<u64> {
        self.db
            .view(|tx| Ok(tx.get::<Payload>(*addr)?.map(|p| p.refcnt)))
            .unwrap()
    }
}

#[test]
fn reserve_size_counts_stamped_entries_not_addresses() {
    // Same content address stamped under N distinct batches must leave the
    // reserve size == N (one Entry row per (batchID, stampIndex, address)),
    // NOT 1. The reserve size feeds storage_radius / committedDepth, which is
    // consensus-committed.
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
    // One shared, refcounted payload: N entries, one body, refcnt == N.
    assert_eq!(fx.row_count::<Payload>(), 1, "one shared content payload");
    assert_eq!(fx.payload_refcnt(&addr), Some(ids.len() as u64));
}

#[test]
fn newest_timestamp_wins_full_index_keying() {
    // Two stamps for the SAME (batchID, full 8-byte stampIndex): a newer
    // timestamp displaces the older entry (size stays 1, slot updated); an
    // EQUAL timestamp REJECTS (prev >= curr); an OLDER timestamp REJECTS.
    // Distinct indices in the same bucket are different slots and both admit
    // (the gate-refuted bucket-only keying must NOT collapse them).
    let fx = Fixture::new();
    let id = fx.batch_id();
    let (chunk, addr) = content_chunk_in_bucket0(2);

    // First stamp at index 0, timestamp 100.
    fx.put(&chunk, &addr, id, 0, 100).unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 1);

    // Newer timestamp on the SAME slot: restamp, size unchanged.
    fx.put(&chunk, &addr, id, 0, 200).unwrap();
    assert_eq!(
        fx.reserve.count().unwrap(),
        1,
        "restamp displaces the old entry; size unchanged"
    );
    // The surviving entry carries the newer stamp (timestamp 200).
    let got = fx.reserve.get(&addr).unwrap().expect("present");
    assert_eq!(got.stamp().expect("stamped").timestamp(), 200);

    // Equal timestamp on the same slot: REJECT, nothing changes.
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

    // Older timestamp on the same slot: REJECT.
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

    // A DISTINCT index in the same bucket is a different slot: it admits and
    // adds a second entry (bucket-only keying would wrongly collapse these).
    fx.put(&chunk, &addr, id, 1, 50).unwrap();
    assert_eq!(
        fx.reserve.count().unwrap(),
        2,
        "distinct stampIndex in the same bucket is a separate slot"
    );
}

#[test]
fn refcounted_payload_survives_partial_eviction() {
    // Same content under two batches: one Payload row, refcnt 2. The
    // second-batch put must NOT rewrite the body (refcnt bump only). Evicting
    // one entry leaves the body present (refcnt 1) and the surviving entry
    // still resolves.
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

    // Evict the furthest entry: one entry goes, the body survives (refcnt 1).
    let evicted = fx.reserve.evict_furthest().unwrap();
    assert_eq!(evicted, Some(addr));
    assert_eq!(fx.reserve.count().unwrap(), 1, "one entry removed");
    assert_eq!(
        fx.payload_refcnt(&addr),
        Some(1),
        "shared body survives partial eviction"
    );
    // The surviving entry still resolves to the chunk.
    let got = fx.reserve.get(&addr).unwrap().expect("survivor present");
    assert_eq!(got.address(), &addr);

    // Evicting the last entry drops the body entirely (no tombstone).
    fx.reserve.evict_furthest().unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 0);
    assert_eq!(fx.payload_refcnt(&addr), None, "last entry drops the body");
}

#[test]
fn restamp_to_different_address_cleans_displaced_rows() {
    // A restamp re-points the (batchID, stampIndex) slot at a DIFFERENT
    // content address. The displaced entry's rows must be deleted using the
    // DISPLACED address's proximity (regression guard for the orphaned-row /
    // leaked-refcount bug), and its payload refcount decremented exactly once.
    let fx = Fixture::new();
    let id = fx.batch_id();
    // Two distinct addresses, both in bucket 0, so they share index slot 0.
    let (chunk_a, addr_a) = content_chunk_in_bucket0(10);
    let (chunk_b, addr_b) = content_chunk_in_bucket0(20);
    assert_ne!(addr_a, addr_b, "fixtures must be distinct addresses");

    // Stamp A into slot (id, index 0) at timestamp 100.
    fx.put(&chunk_a, &addr_a, id, 0, 100).unwrap();
    assert_eq!(fx.reserve.count().unwrap(), 1);
    assert!(fx.reserve.contains(&addr_a));

    // Restamp the SAME slot onto address B at a newer timestamp: A is
    // displaced, B admitted. Size unchanged (one displaced, one added).
    fx.put(&chunk_b, &addr_b, id, 0, 200).unwrap();
    assert_eq!(
        fx.reserve.count().unwrap(),
        1,
        "restamp to a different address displaces A and admits B"
    );

    // A's body and all its index rows are gone (no orphaned rows, no leaked
    // refcount); B is present and resolves.
    assert!(!fx.reserve.contains(&addr_a), "displaced A body removed");
    assert_eq!(fx.payload_refcnt(&addr_a), None, "A refcount not leaked");
    assert!(fx.reserve.contains(&addr_b), "B present");
    assert_eq!(fx.payload_refcnt(&addr_b), Some(1));

    // Exactly one of each index row remains (B's), no A residue.
    assert_eq!(fx.row_count::<Entry>(), 1, "one Entry row (B)");
    assert_eq!(fx.row_count::<BatchGroup>(), 1, "one BatchGroup row (B)");
    assert_eq!(fx.row_count::<Replay>(), 1, "one Replay row (B)");
    assert_eq!(fx.row_count::<Payload>(), 1, "one Payload row (B)");
}

#[test]
fn get_returns_exact_admitting_stamp() {
    // get() must surface the PRECISE stamp the entry was admitted with
    // (stored per entry), byte-for-byte, not a stamp re-loaded by batchID
    // alone. An inclusion proof carries exactly this stamp.
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
    // remove() must delete every row of every stamped entry for an address
    // across all six tables, leaving no tombstone. Store the same content
    // under two batches (two entries, one shared payload), then remove and
    // assert all tables are empty.
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
    // The arbiter slots for both batches are cleared, so a later older stamp
    // is admitted afresh (slot did not pin a stale newest).
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
    // A stamp referencing a batch the node does not know is refused, and a
    // stampless put is invalid; neither writes anything.
    let fx = Fixture::new();
    let (chunk, addr) = content_chunk_in_bucket0(6);

    // Stampless.
    let err = fx
        .reserve
        .put(CachedChunk::new(chunk.clone(), None))
        .unwrap_err();
    assert!(matches!(err, SwarmError::InvalidChunk { .. }));
    assert!(!fx.reserve.contains(&addr));

    // Unknown batch: sign under a batch id the store does not hold.
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
    // The Replay table feeds the redistribution/sync consumer in per-bin
    // insertion order, surfacing the precise (address, batch, stampHash) of
    // each stamped entry without a body read.
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

// sample-at-most-once (a chunk under N batches contributes at most one slot to
// a sample, equal transformed address collapses, CAC beats SOC) is a SAMPLER
// property, not a reserve one: the reserve exposes the per-entry Replay
// projection (address, batch, stampHash, chunk_type) the sampler consumes, but
// the collapse is performed by the sampler (PR-F), not here. The reserve-side
// guarantee that the projection is per-entry and carries the chunk type is
// covered by bin_scan_replays_entries_in_insertion_order plus the ReplayValue
// schema; the collapse itself is asserted in PR-F.
#[test]
#[ignore = "sample-at-most-once collapse is a PR-F sampler property; reserve only supplies the per-entry Replay projection"]
fn sample_collapses_duplicate_transformed_address() {
    // Intentionally deferred to PR-F (sampler). See the comment above: the
    // reserve's responsibility (a per-entry, chunk-typed Replay projection) is
    // covered by the bin-scan test; the at-most-once collapse over transformed
    // addresses belongs to the sampler that consumes that projection.
}
