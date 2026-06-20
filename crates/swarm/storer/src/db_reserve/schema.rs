//! The six-table schema: the `table!` definitions plus every compound-key and
//! value codec they bind.
//!
//! All compound keys are big-endian so the byte order of the encoded key is the
//! `(field, field, ...)` lexicographic order; the orderings are pinned by the
//! tests at the foot of this module.

use alloy_primitives::{B256, keccak256};
use nectar_postage::Stamp;
use nectar_primitives::{Bin, ChunkAddress};
use serde::{Deserialize, Serialize};
use vertex_storage::{DatabaseError, Decode, Encode, table};

use vertex_swarm_primitives::BatchId;

// -------------------------------------------------------------------------
// Tables (the six-table per-stamped-entry schema).
// -------------------------------------------------------------------------

// Refcounted content payload: `addr -> (refcnt, typed_bytes)`.
//
// One row per distinct *address* (not per entry). The body is the type-tagged
// `AnyChunk` encoding, shared by every stamped entry of that content; the
// refcount is the number of live entries referencing it. Present iff refcnt >=
// 1. Declared uncompressed on the assumption that chunk bodies are
// arbitrary/encrypted, so compression would cost CPU without saving space.
// That assumption is untested: for some workloads (for example unencrypted
// content-addressed bodies) compression may still recover space. Making this a
// configuration option (with an uncompressed-to-compressed migration for
// analysis) is tracked as a follow-up, because `compressed` is a compile-time
// `table!` parameter and a runtime toggle is a storage-layer change, not a
// reserve change.
table!(pub(crate) Payload, "reserve_payload", ChunkAddress, PayloadValue, compressed = false);

// Per-stamped-entry primary index: `(po, batch, stampHash, addr) -> EntryValue`.
//
// One row per stamped entry; the reserve size is the count of this table. Keyed
// proximity-major so the furthest entry (smallest po) is the table's first key
// and a proximity-bin's entries are contiguous. The value carries the bin
// sequence (to address the matching `Replay` row on removal) and the precise
// stamp (so `get` and inclusion proofs use the exact admitting stamp).
table!(pub(crate) Entry, "reserve_entry", EntryKey, EntryValue, compressed = false);

// Batch grouping: `(batch, po, addr, stampHash) -> ()`.
//
// One row per stamped entry, batch-major then bin then address, so a prefix
// cursor over a batch yields its entries bin-ascending for batch eviction.
table!(pub(crate) BatchGroup, "reserve_batch_group", BatchGroupKey, (), compressed = false);

// Insertion-order replay: `(bin, binid) -> ReplayValue`.
//
// The append-only per-bin index a redistribution/sync consumer replays. Keyed
// `(bin, binid)` big-endian so a bin's rows are contiguous and ascending by
// sequence. The value projects what a consumer needs without a body read.
table!(pub(crate) Replay, "reserve_replay", ReplayKey, ReplayValue, compressed = false);

// Per-bin insertion cursor: `bin -> u64`.
//
// The highest sequence assigned in each bin so far; the next insertion writes
// `cursor + 1`. Never rewound on eviction, so its value is the lifetime count
// of entries ever admitted to the bin and sequences are never reused (the
// guarantee a sync consumer's resume point depends on). Exhausting the `u64`
// would require more than 1.8e19 admissions into a single bin, which is
// unreachable in any node lifetime; the increment site in `store::put` surfaces
// the impossible overflow as a hard error rather than wrapping (see there).
table!(pub(crate) BinCounter, "reserve_bin_counter", BinKey, u64, compressed = false);

// Stamp-index arbiter slot: `(batch, stampIndex_be8) -> (timestamp, stampHash,
// addr)`.
//
// This is *the* postage stamp-index table. The reserve does not re-invoke
// `table!` to declare a second type-level handle to the same physical table
// (which would be free to drift in name or value codec and silently
// desynchronise the on-disk slot): it imports the single `StampIndexTable`
// handle the postage crate (PR-C) owns and re-exports. The name and the
// `(key, value)` codec binding therefore live in exactly one place. The reserve
// only changes *when* it is written: rather than calling `DbStampIndexArbiter`,
// it runs the same `decide` arbitration *inside* its own atomic put transaction
// so admission and the six-table write commit together.

// -------------------------------------------------------------------------
// Key newtypes and value records.
// -------------------------------------------------------------------------

/// Newtype key wrapping a [`Bin`] for [`BinCounter`].
///
/// [`Bin`] is a foreign (nectar) type, so the vertex-storage codecs cannot be
/// implemented for it directly (orphan rule). Carries the single-byte encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BinKey(pub u8);

impl BinKey {
    pub(crate) fn from_bin(bin: Bin) -> Self {
        Self(bin.get())
    }
}

impl Encode for BinKey {
    type Encoded = [u8; 1];

    fn encode(self) -> Self::Encoded {
        [self.0]
    }
}

impl Decode for BinKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 1] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(bytes[0]))
    }
}

/// The refcounted content payload value: `(refcnt, typed_bytes)`.
///
/// `typed_bytes` is the type-tagged [`AnyChunk`] encoding (no stamp), shared by
/// every stamped entry of the content. `refcnt` is the number of live entries
/// referencing it; the row is deleted when it reaches zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PayloadValue {
    /// Number of live stamped entries referencing this content.
    pub(crate) refcnt: u64,
    /// The type-tagged chunk body, shared across all referencing entries.
    pub(crate) typed_bytes: Vec<u8>,
}

/// Compound key `(po, batch, stampHash, addr)` for [`Entry`].
///
/// Big-endian `[po: 1][batch: 32][stampHash: 32][addr: 32]` (97 bytes): the byte
/// order is proximity-major, so the globally furthest entry (smallest po) is the
/// table's first key and a proximity bin's entries are contiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct EntryKey {
    pub(crate) po: u8,
    pub(crate) batch: BatchId,
    pub(crate) stamp_hash: B256,
    pub(crate) addr: ChunkAddress,
}

impl EntryKey {
    pub(crate) fn new(po: u8, batch: BatchId, stamp_hash: B256, addr: ChunkAddress) -> Self {
        Self {
            po,
            batch,
            stamp_hash,
            addr,
        }
    }
}

impl Encode for EntryKey {
    type Encoded = [u8; 97];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 97];
        out[0] = self.po;
        out[1..33].copy_from_slice(self.batch.as_slice());
        out[33..65].copy_from_slice(self.stamp_hash.as_slice());
        out[65..].copy_from_slice(self.addr.as_slice());
        out
    }
}

impl Decode for EntryKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 97] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let batch: [u8; 32] = bytes[1..33].try_into().map_err(|_| DatabaseError::Decode)?;
        let hash: [u8; 32] = bytes[33..65]
            .try_into()
            .map_err(|_| DatabaseError::Decode)?;
        let addr: [u8; 32] = bytes[65..].try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self {
            po: bytes[0],
            batch: BatchId::from(batch),
            stamp_hash: B256::from(hash),
            addr: ChunkAddress::from(addr),
        })
    }
}

/// The per-entry value: the bin sequence the entry landed at, plus the precise
/// stamp the entry was admitted with (canonical 113-byte encoding).
///
/// The stamp is stored *per entry*, not in the shared payload, because distinct
/// entries of the same content carry distinct stamps. Holding the exact stamp
/// lets [`get`](crate::DbReserve) reconstruct a stamped chunk and lets a
/// later inclusion proof carry the precise stamp the slot was won with, rather
/// than re-loading one by batch id alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EntryValue {
    /// The bin and sequence this entry occupies in [`Replay`].
    pub(crate) bin: u8,
    pub(crate) binid: u64,
    /// The exact admitting stamp, canonical 113-byte encoding.
    pub(crate) stamp_bytes: Vec<u8>,
}

/// Compound key `(batch, po, addr, stampHash)` for [`BatchGroup`].
///
/// Big-endian `[batch: 32][po: 1][addr: 32][stampHash: 32]` (97 bytes): grouped
/// by batch then bin then address, so a batch's entries are a contiguous prefix
/// ascending by bin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BatchGroupKey {
    pub(crate) batch: BatchId,
    pub(crate) po: u8,
    pub(crate) addr: ChunkAddress,
    pub(crate) stamp_hash: B256,
}

impl BatchGroupKey {
    pub(crate) fn new(batch: BatchId, po: u8, addr: ChunkAddress, stamp_hash: B256) -> Self {
        Self {
            batch,
            po,
            addr,
            stamp_hash,
        }
    }
}

impl Encode for BatchGroupKey {
    type Encoded = [u8; 97];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 97];
        out[..32].copy_from_slice(self.batch.as_slice());
        out[32] = self.po;
        out[33..65].copy_from_slice(self.addr.as_slice());
        out[65..].copy_from_slice(self.stamp_hash.as_slice());
        out
    }
}

impl Decode for BatchGroupKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 97] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let batch: [u8; 32] = bytes[..32].try_into().map_err(|_| DatabaseError::Decode)?;
        let addr: [u8; 32] = bytes[33..65]
            .try_into()
            .map_err(|_| DatabaseError::Decode)?;
        let hash: [u8; 32] = bytes[65..].try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self {
            batch: BatchId::from(batch),
            po: bytes[32],
            addr: ChunkAddress::from(addr),
            stamp_hash: B256::from(hash),
        })
    }
}

/// Compound key `(bin, binid)` for [`Replay`].
///
/// Big-endian `[bin: 1][binid: 8]` so a bin's rows are contiguous and ascending
/// by sequence, exactly the order the bin scan walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct ReplayKey {
    pub(crate) bin: u8,
    pub(crate) binid: u64,
}

impl ReplayKey {
    pub(crate) fn new(bin: u8, binid: u64) -> Self {
        Self { bin, binid }
    }
}

impl Encode for ReplayKey {
    type Encoded = [u8; 9];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 9];
        out[0] = self.bin;
        out[1..].copy_from_slice(&self.binid.to_be_bytes());
        out
    }
}

impl Decode for ReplayKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 9] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let mut id = [0u8; 8];
        id.copy_from_slice(&bytes[1..]);
        Ok(Self {
            bin: bytes[0],
            binid: u64::from_be_bytes(id),
        })
    }
}

/// The insertion-order replay value: a flat projection of the stamped entry that
/// landed at a `(bin, binid)`, including the chunk type so a sampler resolves the
/// CAC-beats-SOC tie without a body read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReplayValue {
    pub(crate) address: ChunkAddress,
    pub(crate) batch_id: BatchId,
    pub(crate) stamp_hash: B256,
    /// The chunk type id ([`ChunkTypeId::as_u8`]): 0 = content, 1 = single-owner.
    pub(crate) chunk_type: u8,
}

/// A stable hash of the exact stamp version that admitted an entry.
///
/// Keccak over the stamp's canonical 113-byte serialization, so a re-stamp of
/// the same content under a different batch/index/timestamp yields a different
/// hash. The stamp-entry identity is `(batchID, stampIndex, address)`, but the
/// stamp hash is a compact, collision-resistant stand-in carried in the index
/// rows and is what a consumer compares to detect a re-stamp.
pub(crate) fn stamp_hash(stamp: &Stamp) -> B256 {
    keccak256(stamp.to_bytes())
}

// ---------------------------------------------------------------------------
// Key-codec ordering tests (pure, no database).
// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions over known-bounds fixtures"
)]
mod key_codec_tests {
    use super::*;

    #[test]
    fn entry_key_round_trips_and_orders_proximity_major() {
        // Round-trip.
        let k = EntryKey::new(
            7,
            BatchId::repeat_byte(0x11),
            B256::repeat_byte(0x22),
            ChunkAddress::from([0x33u8; 32]),
        );
        assert_eq!(EntryKey::decode(k.encode().as_ref()).unwrap(), k);

        // Proximity-major: a smaller po sorts first regardless of the trailing
        // fields, so `first()` over the table is always the furthest entry.
        let far = EntryKey::new(
            1,
            BatchId::repeat_byte(0xff),
            B256::repeat_byte(0xff),
            ChunkAddress::from([0xffu8; 32]),
        )
        .encode();
        let near =
            EntryKey::new(2, BatchId::ZERO, B256::ZERO, ChunkAddress::from([0u8; 32])).encode();
        assert!(far < near, "smaller proximity order sorts first");

        // Within one po, ordering is by (batch, stampHash, addr).
        let po = 5u8;
        let a =
            EntryKey::new(po, BatchId::ZERO, B256::ZERO, ChunkAddress::from([0u8; 32])).encode();
        let b = EntryKey::new(
            po,
            BatchId::repeat_byte(0x01),
            B256::ZERO,
            ChunkAddress::from([0u8; 32]),
        )
        .encode();
        assert!(a < b, "same po orders by batch next");
    }

    #[test]
    fn batch_group_key_orders_batch_major_then_bin() {
        let k = BatchGroupKey::new(
            BatchId::repeat_byte(0xab),
            9,
            ChunkAddress::from([0xcdu8; 32]),
            B256::repeat_byte(0xef),
        );
        assert_eq!(BatchGroupKey::decode(k.encode().as_ref()).unwrap(), k);

        // A batch's entries are a contiguous prefix, ascending by bin.
        let batch = BatchId::repeat_byte(0x07);
        let lo = BatchGroupKey::new(batch, 1, ChunkAddress::from([0u8; 32]), B256::ZERO).encode();
        let hi = BatchGroupKey::new(batch, 2, ChunkAddress::from([0u8; 32]), B256::ZERO).encode();
        let other = BatchGroupKey::new(
            BatchId::repeat_byte(0x08),
            0,
            ChunkAddress::from([0u8; 32]),
            B256::ZERO,
        )
        .encode();
        assert!(lo < hi, "same batch orders by bin");
        assert!(hi < other, "lower batch sorts before higher batch");
    }

    #[test]
    fn replay_key_orders_bin_major_then_sequence() {
        let k = ReplayKey::new(3, 0x0102_0304_0506_0708);
        assert_eq!(ReplayKey::decode(k.encode().as_ref()).unwrap(), k);

        let a = ReplayKey::new(1, 9).encode();
        let b = ReplayKey::new(1, 10).encode();
        let c = ReplayKey::new(2, 0).encode();
        assert!(a < b, "same bin orders by sequence");
        assert!(b < c, "lower bin sorts before higher bin");
    }

    #[test]
    fn bin_key_round_trips() {
        let bk = BinKey::from_bin(Bin::new(5).unwrap());
        assert_eq!(BinKey::decode(bk.encode().as_ref()).unwrap(), bk);
    }
}
