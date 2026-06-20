//! Stamp-index arbiter: newest-timestamp-wins per slot, keyed by the full
//! `(batchID, 8-byte stampIndex)`.
//!
//! An issuer may re-use a slot, so the reserve must decide whether an incoming
//! stamp is the newest seen for its `(batch, index)` slot (admit, displacing
//! the previous occupant) or stale (reject).
//!
//! Consensus-observable rule: ordering is by the stamp timestamp (8-byte
//! big-endian). `prev >= curr` rejects, so equal timestamps reject (a
//! re-presentation must not overwrite). The comparison keys on the full
//! `(batchID, stampIndex)`: two indices within the same bucket are distinct
//! slots and both admit.
//!
//! Each slot stores `(timestamp, stamp_hash, address)` so a displacement can
//! report exactly the chunk to evict without a second lookup.

use alloy_primitives::B256;
use nectar_postage::{BatchId, StampIndex};
use nectar_primitives::ChunkAddress;
use std::sync::Arc;
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Decode, Encode, Table, table};

// Stamp-index table: `(batchID, stampIndex_be8) -> (timestamp, stampHash, addr)`.
// The key is laid out big-endian as `[batch: 32][index: 8]` so byte order
// matches `(batchID, stampIndex)` lexicographic order and a batch's slots are
// contiguous and ascending. The value is a tiny fixed record (8 + 32 + 32),
// not worth compressing.
//
// This handle is `pub` and re-exported so any code addressing the same physical
// table imports one handle rather than re-invoking `table!` (which would create
// a second type-level binding free to drift in name or codec). The name and
// codec binding live only here.
table!(pub StampIndexTable, "postage_stamp_index", StampSlotKey, StampIndexEntry, compressed = false);

/// The full per-slot key: a postage batch and a stamp index within it.
///
/// The 8-byte stamp index is `[bucket: 4][index: 4]` big-endian, so the 40-byte
/// key sorts batch-major, then bucket, then index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StampSlotKey {
    pub batch_id: BatchId,
    pub stamp_index: StampIndex,
}

impl StampSlotKey {
    pub const fn new(batch_id: BatchId, stamp_index: StampIndex) -> Self {
        Self {
            batch_id,
            stamp_index,
        }
    }
}

// `StampIndex` is foreign and unordered, so ordering is defined on the canonical
// 40-byte encoding, which equals the on-disk key order (batch, bucket, index).
impl Ord for StampSlotKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.encode().cmp(&other.encode())
    }
}

impl PartialOrd for StampSlotKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// Hand-rolled codec so the on-disk byte order is exactly `[batch: 32][index_be8: 8]`.
impl Encode for StampSlotKey {
    type Encoded = [u8; 40];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 40];
        out[..32].copy_from_slice(self.batch_id.as_slice());
        out[32..].copy_from_slice(&self.stamp_index.to_be_bytes());
        out
    }
}

impl Decode for StampSlotKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 40] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let batch: [u8; 32] = bytes[..32].try_into().map_err(|_| DatabaseError::Decode)?;
        let index: [u8; 8] = bytes[32..].try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self {
            batch_id: BatchId::from(batch),
            stamp_index: StampIndex::from_be_bytes(index),
        })
    }
}

// The `Key` blanket bound requires `serde`, though the storage path uses the
// `Encode`/`Decode` codec above. Serialised as a `([u8; 32], [u8; 8])` tuple
// because serde implements arrays only up to length 32.
impl serde::Serialize for StampSlotKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let parts: ([u8; 32], [u8; 8]) = (self.batch_id.0, self.stamp_index.to_be_bytes());
        serde::Serialize::serialize(&parts, serializer)
    }
}

impl<'de> serde::Deserialize<'de> for StampSlotKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (batch, index): ([u8; 32], [u8; 8]) = serde::Deserialize::deserialize(deserializer)?;
        Ok(Self {
            batch_id: BatchId::from(batch),
            stamp_index: StampIndex::from_be_bytes(index),
        })
    }
}

/// The occupant of a stamp-index slot: the stamp version recorded for it.
///
/// Stored as the [`StampIndexTable`] value. The timestamp is kept raw 8-byte
/// big-endian so the ordering comparison is byte-exact with the wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StampIndexEntry {
    /// The stamp timestamp, raw 8-byte big-endian (as it appears on the wire).
    pub timestamp: [u8; 8],
    /// Keccak over the canonical 113-byte stamp: the exact stamp version, so the
    /// caller can evict precisely the chunk this stamp admitted.
    pub stamp_hash: B256,
    pub address: ChunkAddress,
}

impl StampIndexEntry {
    pub const fn new(timestamp: [u8; 8], stamp_hash: B256, address: ChunkAddress) -> Self {
        Self {
            timestamp,
            stamp_hash,
            address,
        }
    }

    #[inline]
    pub const fn timestamp_u64(&self) -> u64 {
        u64::from_be_bytes(self.timestamp)
    }
}

/// An incoming stamped-chunk presentation to arbitrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncomingStamp {
    pub batch_id: BatchId,
    pub stamp_index: StampIndex,
    /// The stamp timestamp, raw 8-byte big-endian.
    pub timestamp: [u8; 8],
    /// Keccak over the canonical 113-byte stamp.
    pub stamp_hash: B256,
    pub address: ChunkAddress,
}

impl IncomingStamp {
    pub const fn new(
        batch_id: BatchId,
        stamp_index: StampIndex,
        timestamp: [u8; 8],
        stamp_hash: B256,
        address: ChunkAddress,
    ) -> Self {
        Self {
            batch_id,
            stamp_index,
            timestamp,
            stamp_hash,
            address,
        }
    }

    fn slot(&self) -> StampSlotKey {
        StampSlotKey::new(self.batch_id, self.stamp_index)
    }

    /// The record this stamp would leave in its slot if admitted.
    pub fn entry(&self) -> StampIndexEntry {
        StampIndexEntry::new(self.timestamp, self.stamp_hash, self.address)
    }
}

/// The slot's previous occupant, reported on admission so the caller can evict
/// exactly the chunk it admitted (by `address` and `stamp_hash`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplacedEntry {
    pub address: ChunkAddress,
    pub stamp_hash: B256,
    /// Raw 8-byte big-endian.
    pub timestamp: [u8; 8],
}

/// Why an incoming stamp was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The slot holds a stamp whose timestamp is `>=` the incoming one. Equal
    /// rejects, older rejects.
    NotNewer { stored: [u8; 8], incoming: [u8; 8] },
}

/// The arbiter's verdict for an incoming stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arbitration {
    /// Admitted as the newest seen for its slot. `displaced` is the previous
    /// occupant to evict, or `None` if the slot was empty.
    Admit { displaced: Option<DisplacedEntry> },
    /// Rejected as stale.
    Reject { reason: RejectReason },
}

/// Pure, storage-free core of the rule: decide the verdict for `incoming`
/// against the slot's current occupant (`stored`, `None` if empty).
///
/// Does not write; the caller persists the new entry on [`Arbitration::Admit`]
/// inside the atomic put transaction.
pub fn decide(stored: Option<&StampIndexEntry>, incoming: &IncomingStamp) -> Arbitration {
    match stored {
        None => Arbitration::Admit { displaced: None },
        Some(prev) => {
            // Raw big-endian compare, byte-exact with the wire timestamp;
            // `prev >= curr` rejects.
            if prev.timestamp >= incoming.timestamp {
                Arbitration::Reject {
                    reason: RejectReason::NotNewer {
                        stored: prev.timestamp,
                        incoming: incoming.timestamp,
                    },
                }
            } else {
                Arbitration::Admit {
                    displaced: Some(DisplacedEntry {
                        address: prev.address,
                        stamp_hash: prev.stamp_hash,
                        timestamp: prev.timestamp,
                    }),
                }
            }
        }
    }
}

/// Errors from the stamp-index arbiter's storage operations.
#[derive(Debug, thiserror::Error)]
pub enum StampIndexError {
    /// An error from the underlying database.
    #[error("stamp-index arbiter database error: {0}")]
    Database(#[from] DatabaseError),
}

/// The object-safe arbiter interface.
pub trait StampIndexArbiter {
    /// Arbitrate `incoming` against its slot. On [`Arbitration::Admit`] the slot
    /// is updated atomically before returning; on [`Arbitration::Reject`] it is
    /// left untouched.
    fn arbitrate(&self, incoming: &IncomingStamp) -> Result<Arbitration, StampIndexError>;

    fn get(&self, slot: &StampSlotKey) -> Result<Option<StampIndexEntry>, StampIndexError>;
}

/// A [`StampIndexArbiter`] backed by a [`StampIndexTable`].
///
/// Each arbitration is a single read-then-conditional-write transaction, so the
/// slot never observes a partial update and the reported displaced entry is
/// exactly the one removed.
pub struct DbStampIndexArbiter<DB: Database> {
    db: Arc<DB>,
}

impl<DB: Database> DbStampIndexArbiter<DB> {
    /// Ensures the stamp-index table exists so the read path works on a fresh
    /// database.
    pub fn new(db: Arc<DB>) -> Result<Self, StampIndexError> {
        db.update(|tx| tx.ensure_table(StampIndexTable::NAME))?;
        Ok(Self { db })
    }

    pub fn database(&self) -> &Arc<DB> {
        &self.db
    }
}

impl<DB: Database> StampIndexArbiter for DbStampIndexArbiter<DB> {
    fn arbitrate(&self, incoming: &IncomingStamp) -> Result<Arbitration, StampIndexError> {
        let slot = incoming.slot();
        // Read-then-conditional-write in one synchronous transaction, so the
        // slot cannot change between the read and the write.
        let verdict = self.db.update(|tx| {
            let stored = tx.get::<StampIndexTable>(slot)?;
            let verdict = decide(stored.as_ref(), incoming);
            if let Arbitration::Admit { .. } = verdict {
                tx.put::<StampIndexTable>(slot, incoming.entry())?;
            }
            Ok(verdict)
        })?;
        Ok(verdict)
    }

    fn get(&self, slot: &StampSlotKey) -> Result<Option<StampIndexEntry>, StampIndexError> {
        Ok(self.db.view(|tx| tx.get::<StampIndexTable>(*slot))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use vertex_storage_redb::RedbDatabase;

    fn arbiter() -> DbStampIndexArbiter<RedbDatabase> {
        let db = RedbDatabase::in_memory().unwrap().into_arc();
        DbStampIndexArbiter::new(db).unwrap()
    }

    fn batch(b: u8) -> BatchId {
        B256::repeat_byte(b)
    }

    fn addr(b: u8) -> ChunkAddress {
        ChunkAddress::from([b; 32])
    }

    fn hash(b: u8) -> B256 {
        B256::repeat_byte(b)
    }

    fn ts(v: u64) -> [u8; 8] {
        v.to_be_bytes()
    }

    fn incoming(
        batch_id: BatchId,
        idx: StampIndex,
        timestamp: u64,
        stamp_hash: B256,
        address: ChunkAddress,
    ) -> IncomingStamp {
        IncomingStamp::new(batch_id, idx, ts(timestamp), stamp_hash, address)
    }

    #[test]
    fn empty_slot_admits_with_nothing_displaced() {
        let a = arbiter();
        let inc = incoming(batch(1), StampIndex::new(3, 7), 100, hash(0xaa), addr(0xa1));
        let verdict = a.arbitrate(&inc).unwrap();
        assert_eq!(verdict, Arbitration::Admit { displaced: None });
        let slot = StampSlotKey::new(batch(1), StampIndex::new(3, 7));
        assert_eq!(
            a.get(&slot).unwrap(),
            Some(StampIndexEntry::new(ts(100), hash(0xaa), addr(0xa1)))
        );
    }

    #[test]
    fn newer_timestamp_displaces_and_returns_previous() {
        let a = arbiter();
        let idx = StampIndex::new(3, 7);
        let first = incoming(batch(1), idx, 100, hash(0xaa), addr(0xa1));
        a.arbitrate(&first).unwrap();

        let second = incoming(batch(1), idx, 200, hash(0xbb), addr(0xb2));
        let verdict = a.arbitrate(&second).unwrap();
        assert_eq!(
            verdict,
            Arbitration::Admit {
                displaced: Some(DisplacedEntry {
                    address: addr(0xa1),
                    stamp_hash: hash(0xaa),
                    timestamp: ts(100),
                }),
            }
        );
        let slot = StampSlotKey::new(batch(1), idx);
        assert_eq!(
            a.get(&slot).unwrap(),
            Some(StampIndexEntry::new(ts(200), hash(0xbb), addr(0xb2)))
        );
    }

    #[test]
    fn equal_timestamp_rejects_and_leaves_slot_untouched() {
        let a = arbiter();
        let idx = StampIndex::new(3, 7);
        let first = incoming(batch(1), idx, 100, hash(0xaa), addr(0xa1));
        a.arbitrate(&first).unwrap();

        // Same timestamp, different hash/address: must reject (prev >= curr).
        let dup = incoming(batch(1), idx, 100, hash(0xbb), addr(0xb2));
        let verdict = a.arbitrate(&dup).unwrap();
        assert_eq!(
            verdict,
            Arbitration::Reject {
                reason: RejectReason::NotNewer {
                    stored: ts(100),
                    incoming: ts(100),
                },
            }
        );
        let slot = StampSlotKey::new(batch(1), idx);
        assert_eq!(
            a.get(&slot).unwrap(),
            Some(StampIndexEntry::new(ts(100), hash(0xaa), addr(0xa1)))
        );
    }

    #[test]
    fn older_timestamp_rejects_and_leaves_slot_untouched() {
        let a = arbiter();
        let idx = StampIndex::new(3, 7);
        let first = incoming(batch(1), idx, 200, hash(0xaa), addr(0xa1));
        a.arbitrate(&first).unwrap();

        let older = incoming(batch(1), idx, 100, hash(0xbb), addr(0xb2));
        let verdict = a.arbitrate(&older).unwrap();
        assert_eq!(
            verdict,
            Arbitration::Reject {
                reason: RejectReason::NotNewer {
                    stored: ts(200),
                    incoming: ts(100),
                },
            }
        );
        let slot = StampSlotKey::new(batch(1), idx);
        assert_eq!(
            a.get(&slot).unwrap(),
            Some(StampIndexEntry::new(ts(200), hash(0xaa), addr(0xa1)))
        );
    }

    #[test]
    fn distinct_indices_same_bucket_both_admit() {
        // Same bucket, different within-bucket index = different slots, both
        // admit. The full (batch, stampIndex) key keeps them independent.
        let a = arbiter();
        let bucket = 42;
        let one = incoming(
            batch(1),
            StampIndex::new(bucket, 0),
            100,
            hash(0x01),
            addr(0x11),
        );
        let two = incoming(
            batch(1),
            StampIndex::new(bucket, 1),
            100,
            hash(0x02),
            addr(0x22),
        );

        assert_eq!(
            a.arbitrate(&one).unwrap(),
            Arbitration::Admit { displaced: None }
        );
        assert_eq!(
            a.arbitrate(&two).unwrap(),
            Arbitration::Admit { displaced: None }
        );

        assert_eq!(
            a.get(&StampSlotKey::new(batch(1), StampIndex::new(bucket, 0)))
                .unwrap(),
            Some(StampIndexEntry::new(ts(100), hash(0x01), addr(0x11)))
        );
        assert_eq!(
            a.get(&StampSlotKey::new(batch(1), StampIndex::new(bucket, 1)))
                .unwrap(),
            Some(StampIndexEntry::new(ts(100), hash(0x02), addr(0x22)))
        );
    }

    #[test]
    fn same_slot_different_batches_are_independent() {
        // The batch id is part of the key: same (bucket, index) under two
        // batches are two slots.
        let a = arbiter();
        let idx = StampIndex::new(5, 9);
        let b1 = incoming(batch(1), idx, 100, hash(0x01), addr(0x11));
        let b2 = incoming(batch(2), idx, 100, hash(0x02), addr(0x22));
        assert_eq!(
            a.arbitrate(&b1).unwrap(),
            Arbitration::Admit { displaced: None }
        );
        assert_eq!(
            a.arbitrate(&b2).unwrap(),
            Arbitration::Admit { displaced: None }
        );
    }

    #[test]
    fn decide_core_matrix() {
        let idx = StampIndex::new(3, 7);
        let inc = incoming(batch(1), idx, 100, hash(0xbb), addr(0xb2));

        // Empty slot: admit, nothing displaced.
        assert_eq!(decide(None, &inc), Arbitration::Admit { displaced: None });

        // Older stored: admit, displacing it.
        let older = StampIndexEntry::new(ts(50), hash(0xaa), addr(0xa1));
        assert_eq!(
            decide(Some(&older), &inc),
            Arbitration::Admit {
                displaced: Some(DisplacedEntry {
                    address: addr(0xa1),
                    stamp_hash: hash(0xaa),
                    timestamp: ts(50),
                }),
            }
        );

        // Equal stored: reject.
        let equal = StampIndexEntry::new(ts(100), hash(0xaa), addr(0xa1));
        assert_eq!(
            decide(Some(&equal), &inc),
            Arbitration::Reject {
                reason: RejectReason::NotNewer {
                    stored: ts(100),
                    incoming: ts(100),
                },
            }
        );

        // Newer stored: reject.
        let newer = StampIndexEntry::new(ts(150), hash(0xaa), addr(0xa1));
        assert_eq!(
            decide(Some(&newer), &inc),
            Arbitration::Reject {
                reason: RejectReason::NotNewer {
                    stored: ts(150),
                    incoming: ts(100),
                },
            }
        );
    }

    #[test]
    fn timestamp_u64_roundtrips_be() {
        let e = StampIndexEntry::new(ts(0x0102_0304_0506_0708), hash(0), addr(0));
        assert_eq!(e.timestamp_u64(), 0x0102_0304_0506_0708);
    }

    #[test]
    fn slot_key_codec_roundtrip() {
        let k = StampSlotKey::new(batch(0x7e), StampIndex::new(0x0102_0304, 0x0506_0708));
        let encoded = k.encode();
        assert_eq!(encoded.len(), 40);
        // The index half is `[bucket: 4][index: 4]` big-endian.
        assert_eq!(&encoded[32..], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(StampSlotKey::decode(encoded.as_ref()).unwrap(), k);
        assert!(StampSlotKey::decode(&[0u8; 39]).is_err());
        assert!(StampSlotKey::decode(&[0u8; 41]).is_err());
    }

    #[test]
    fn slot_key_byte_order_is_batch_then_bucket_then_index() {
        // The encoded key must sort batch-major, then bucket, then index, so a
        // batch's slots are contiguous and ascending.
        let lo = StampSlotKey::new(batch(1), StampIndex::new(0, 0)).encode();
        let mid = StampSlotKey::new(batch(1), StampIndex::new(0, 1)).encode();
        let hi = StampSlotKey::new(batch(1), StampIndex::new(1, 0)).encode();
        let next_batch = StampSlotKey::new(batch(2), StampIndex::new(0, 0)).encode();
        assert!(lo < mid);
        assert!(mid < hi);
        assert!(hi < next_batch);
    }

    #[test]
    fn arbitration_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stampindex.redb");
        let idx = StampIndex::new(3, 7);
        let inc = incoming(batch(1), idx, 100, hash(0xaa), addr(0xa1));
        {
            let db = RedbDatabase::create(&path).unwrap().into_arc();
            let a = DbStampIndexArbiter::new(db).unwrap();
            a.arbitrate(&inc).unwrap();
        }
        // Reopen: the slot survives, so a stale stamp still rejects.
        let db = RedbDatabase::open(&path).unwrap().into_arc();
        let a = DbStampIndexArbiter::new(db).unwrap();
        let stale = incoming(batch(1), idx, 100, hash(0xbb), addr(0xb2));
        assert!(matches!(
            a.arbitrate(&stale).unwrap(),
            Arbitration::Reject { .. }
        ));
    }
}
