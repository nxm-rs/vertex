//! The stamp-index arbiter: newest-timestamp-wins keyed by the full
//! `(batchID, 8-byte stampIndex)`.
//!
//! # What this decides
//!
//! A postage batch is partitioned into *buckets*, and each bucket holds a fixed
//! number of *index* slots. A stamp commits to one slot via its
//! [`StampIndex`], whose canonical 8-byte big-endian encoding is
//! `[bucket: 4][index: 4]` (see [`StampIndex::to_be_bytes`]). The settlement
//! contract lets an issuer *re-use* a slot: a later stamp under the same batch
//! and the same `(bucket, index)` supersedes the earlier one. The reserve must
//! therefore decide, when it sees a stamped chunk, whether that stamp is the
//! newest the node has seen for its slot (admit, possibly displacing the
//! previous occupant) or stale (reject).
//!
//! This module is that decision, and nothing else. It does not touch the
//! reserve, the payload store, or any of the proximity indexes. PR-D's reserve
//! calls into it; here it is a self-contained, independently tested unit.
//!
//! # The rule (consensus-observable)
//!
//! Ordering is by the stamp *timestamp* (an 8-byte big-endian value on the
//! wire), per slot:
//!
//! - **newer** incoming timestamp than the stored one: **admit**, and report
//!   the displaced entry so the caller can evict the chunk it stamped;
//! - **no stored entry** for the slot: **admit** with nothing displaced;
//! - **equal or older** incoming timestamp: **reject**. Equality rejects: a
//!   re-presentation of an already-seen stamp (same timestamp) must not
//!   overwrite, and an older stamp is plainly stale. In bee's words,
//!   `prev >= curr` rejects.
//!
//! The comparison is `prev >= curr` on the raw big-endian timestamp, evaluated
//! against the *full* `(batchID, stampIndex)` key. Keying by the bucket alone
//! is wrong and was refuted at the design gate: two distinct indices within the
//! same bucket are different slots and must both be admissible. The
//! [`distinct_indices_same_bucket_both_admit`](tests) test guards that.
//!
//! # Why a hash and an address travel with the timestamp
//!
//! When a newer stamp displaces an older one, the caller must remove *exactly*
//! the chunk the old stamp admitted, identified by its content address and the
//! precise stamp version (its stamp hash, a keccak over the canonical stamp
//! bytes). Storing `(timestamp, stamp_hash, address)` in the slot lets the
//! arbiter hand back a fully-formed [`DisplacedEntry`] without a second lookup,
//! and lets an inclusion proof later carry the exact stamp a sample slot was
//! won with rather than re-loading one by batch id alone.

use alloy_primitives::B256;
use nectar_postage::{BatchId, StampIndex};
use nectar_primitives::ChunkAddress;
use std::sync::Arc;
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Decode, Encode, Table, table};

// Stamp-index table: `(batchID, stampIndex_be8) -> (timestamp, stampHash, addr)`.
//
// The compound key is laid out big-endian as `[batch: 32][index: 8]` so that
// the byte order matches `(batchID, stampIndex)` lexicographic order: every
// slot of a batch is contiguous and ascending by `(bucket, index)`. The value
// is the data needed to identify and evict the slot's current occupant.
//
// Uncompressed: the value is a tiny fixed record (8 + 32 + 32 bytes) for which
// compression is pure overhead.
//
// This handle is `pub` and re-exported from the crate root *on purpose*. The
// reserve (PR-D) decides admission inside its own atomic put transaction rather
// than calling [`DbStampIndexArbiter`], but it must address the *same* physical
// table. Rather than re-invoke `table!` there (which would create a second
// type-level handle to the same on-disk table, free to drift in name or value
// codec and silently desynchronise it), the reserve imports this single handle.
// The name and the `(key, value)` codec binding therefore live in exactly one
// place, here.
table!(pub StampIndexTable, "postage_stamp_index", StampSlotKey, StampIndexEntry, compressed = false);

/// The full per-slot key: a postage batch and a stamp index within it.
///
/// This is the *full* `(batchID, 8-byte stampIndex)` key the rule keys on, not
/// the bucket alone. The 8-byte stamp index is itself `[bucket: 4][index: 4]`
/// big-endian, so the 40-byte key sorts batch-major, then bucket, then index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StampSlotKey {
    /// The postage batch the slot belongs to.
    pub batch_id: BatchId,
    /// The stamp index (bucket and within-bucket position) within the batch.
    pub stamp_index: StampIndex,
}

impl StampSlotKey {
    /// Construct a slot key from its batch and stamp index.
    pub const fn new(batch_id: BatchId, stamp_index: StampIndex) -> Self {
        Self {
            batch_id,
            stamp_index,
        }
    }
}

// `StampIndex` is a foreign type that implements neither `PartialOrd` nor `Ord`,
// so `StampSlotKey`'s ordering is defined directly on the canonical 40-byte
// encoding. This is also exactly the on-disk key order (batch-major, then
// bucket, then index), so the `Key` bound's `Ord` agrees byte-for-byte with the
// storage layout.
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

// The key codec is hand-rolled (rather than a serde derive) so the on-disk byte
// order is exactly `[batch: 32][index_be8: 8]`. `StampIndex` and `BatchId` are
// foreign types without local codecs, and serde would not pin the ordering, so
// the newtype carries the canonical big-endian layout directly.
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

// `StampSlotKey` is used as a redb table key, which requires `serde` (the `Key`
// blanket bound). It is never actually serialised through serde on the storage
// path - the `Encode`/`Decode` codec above is - but the bound must be
// satisfied. The two halves are serialised as a `([u8; 32], [u8; 8])` tuple:
// serde implements arrays only up to length 32, so the 40-byte encoding cannot
// be a single array, but the batch and index halves are within that limit.
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
/// Stored as the [`StampIndexTable`] value. The timestamp is kept in its raw
/// 8-byte big-endian form so the ordering comparison is byte-exact with the
/// wire representation and identical to bee's `binary.BigEndian.Uint64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StampIndexEntry {
    /// The stamp timestamp, raw 8-byte big-endian (as it appears on the wire).
    pub timestamp: [u8; 8],
    /// Keccak over the canonical 113-byte stamp, identifying the exact stamp
    /// version. Lets the caller evict precisely the chunk this stamp admitted.
    pub stamp_hash: B256,
    /// The content address the stamp was applied to.
    pub address: ChunkAddress,
}

impl StampIndexEntry {
    /// Construct a slot occupant record.
    pub const fn new(timestamp: [u8; 8], stamp_hash: B256, address: ChunkAddress) -> Self {
        Self {
            timestamp,
            stamp_hash,
            address,
        }
    }

    /// The timestamp as a `u64`, decoded from its big-endian bytes.
    #[inline]
    pub const fn timestamp_u64(&self) -> u64 {
        u64::from_be_bytes(self.timestamp)
    }
}

/// An incoming stamped-chunk presentation to arbitrate.
///
/// Carries the full slot identity `(batch_id, stamp_index)` plus the data that
/// would be recorded if it wins: its timestamp, stamp hash and address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncomingStamp {
    /// The postage batch the stamp draws on.
    pub batch_id: BatchId,
    /// The stamp index (bucket and within-bucket position).
    pub stamp_index: StampIndex,
    /// The stamp timestamp, raw 8-byte big-endian.
    pub timestamp: [u8; 8],
    /// Keccak over the canonical 113-byte stamp.
    pub stamp_hash: B256,
    /// The content address the stamp was applied to.
    pub address: ChunkAddress,
}

impl IncomingStamp {
    /// Construct an incoming stamp from its parts.
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

    /// The slot this stamp competes for.
    fn slot(&self) -> StampSlotKey {
        StampSlotKey::new(self.batch_id, self.stamp_index)
    }

    /// The record this stamp would leave in its slot if admitted.
    fn entry(&self) -> StampIndexEntry {
        StampIndexEntry::new(self.timestamp, self.stamp_hash, self.address)
    }
}

/// The entry an admission displaced: the slot's previous occupant.
///
/// When an incoming stamp is newer than the one already in its slot, the older
/// occupant is reported here so the caller can evict exactly the chunk it
/// admitted (by `address` and `stamp_hash`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplacedEntry {
    /// The address the displaced stamp was applied to.
    pub address: ChunkAddress,
    /// The displaced stamp's hash (its precise version).
    pub stamp_hash: B256,
    /// The displaced stamp's timestamp, raw 8-byte big-endian.
    pub timestamp: [u8; 8],
}

/// Why an incoming stamp was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The slot already holds a stamp whose timestamp is greater than or equal
    /// to the incoming one (`prev >= curr`). Equal timestamps reject: a
    /// re-presentation does not overwrite, and an older stamp is stale.
    NotNewer {
        /// The stored (previous) timestamp, big-endian.
        stored: [u8; 8],
        /// The incoming timestamp, big-endian.
        incoming: [u8; 8],
    },
}

/// The arbiter's verdict for an incoming stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arbitration {
    /// The stamp is the newest seen for its slot and is admitted. If a previous
    /// occupant was displaced, it is reported so the caller can evict it; a
    /// `None` means the slot was empty.
    Admit {
        /// The previous occupant, if any, that the admission displaced.
        displaced: Option<DisplacedEntry>,
    },
    /// The stamp is stale (equal or older than the stored one) and is rejected.
    Reject {
        /// Why the stamp was rejected.
        reason: RejectReason,
    },
}

/// Pure, storage-free core of the rule.
///
/// Given the slot's current occupant (`stored`, `None` if empty) and the
/// incoming stamp, decide the verdict. Admission does not itself write; the
/// caller (here [`DbStampIndexArbiter`], in PR-D the reserve, inside its own
/// transaction) persists the new entry when the verdict is
/// [`Arbitration::Admit`].
///
/// This is split out so the comparison can be reused verbatim inside the
/// reserve's atomic put transaction without going through this module's own
/// database handle, keeping the rule defined in exactly one place.
pub fn decide(stored: Option<&StampIndexEntry>, incoming: &IncomingStamp) -> Arbitration {
    match stored {
        None => Arbitration::Admit { displaced: None },
        Some(prev) => {
            // Compare raw big-endian bytes, which orders identically to the
            // decoded u64 and is byte-exact with the wire timestamp. `prev >=
            // curr` rejects (equal rejects, older rejects).
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
///
/// Arbitrate an incoming stamp against the slot it competes for, persisting the
/// new occupant on admission and reporting any displaced one. Object-safe (no
/// generic methods, no `Self`-by-value), so the reserve can hold a
/// `dyn StampIndexArbiter` if it chooses.
pub trait StampIndexArbiter {
    /// Arbitrate `incoming` against its slot.
    ///
    /// On [`Arbitration::Admit`] the slot is updated to the incoming stamp
    /// atomically before returning, and any previous occupant is reported as
    /// `displaced`. On [`Arbitration::Reject`] the slot is left untouched.
    fn arbitrate(&self, incoming: &IncomingStamp) -> Result<Arbitration, StampIndexError>;

    /// Look up the current occupant of a slot, if any.
    fn get(&self, slot: &StampSlotKey) -> Result<Option<StampIndexEntry>, StampIndexError>;
}

/// A [`StampIndexArbiter`] backed by a [`StampIndexTable`] over a
/// `vertex-storage` `Database`.
///
/// Generic over the backend so the same code serves an in-memory database
/// (tests) and an on-disk redb database (production). Each arbitration is a
/// single read-then-conditional-write transaction, so the slot never observes a
/// partial update and the displaced entry reported is exactly the one removed.
pub struct DbStampIndexArbiter<DB: Database> {
    db: Arc<DB>,
}

impl<DB: Database> DbStampIndexArbiter<DB> {
    /// Create an arbiter over a shared database handle, ensuring the stamp-index
    /// table exists so the read path works on a fresh database.
    pub fn new(db: Arc<DB>) -> Result<Self, StampIndexError> {
        db.update(|tx| tx.ensure_table(StampIndexTable::NAME))?;
        Ok(Self { db })
    }

    /// Borrow the shared database handle.
    pub fn database(&self) -> &Arc<DB> {
        &self.db
    }
}

impl<DB: Database> StampIndexArbiter for DbStampIndexArbiter<DB> {
    fn arbitrate(&self, incoming: &IncomingStamp) -> Result<Arbitration, StampIndexError> {
        let slot = incoming.slot();
        // Read-then-conditional-write in one transaction: the verdict is
        // computed from the slot's current occupant and, on admission, the new
        // entry is written before the transaction commits. No await is involved
        // (redb is synchronous), so the slot cannot change between the read and
        // the write.
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

    // --- the exhaustive timestamp matrix, against a fresh slot each time -----

    #[test]
    fn empty_slot_admits_with_nothing_displaced() {
        let a = arbiter();
        let inc = incoming(batch(1), StampIndex::new(3, 7), 100, hash(0xaa), addr(0xa1));
        let verdict = a.arbitrate(&inc).unwrap();
        assert_eq!(verdict, Arbitration::Admit { displaced: None });
        // The slot now holds the incoming stamp.
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
        // The slot now holds the newer stamp.
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
        // The slot still holds the original stamp.
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

    // --- the design-gate regression guard ----------------------------------

    #[test]
    fn distinct_indices_same_bucket_both_admit() {
        // Two stamps in the SAME bucket but DIFFERENT within-bucket indices are
        // DIFFERENT slots. Both must admit. Keying by bucket alone (the refuted
        // design) would have the second reject or displace the first; the full
        // (batch, stampIndex) key keeps them independent.
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
        // Same bucket, same (equal) timestamp, different index: still admits,
        // displacing nothing, because it is a distinct slot.
        assert_eq!(
            a.arbitrate(&two).unwrap(),
            Arbitration::Admit { displaced: None }
        );

        // Both slots are populated independently.
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
        // The full key includes the batch id: the same (bucket, index) under two
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

    // --- the pure decision core, exhaustively ------------------------------

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

    // --- key codec and ordering --------------------------------------------

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
