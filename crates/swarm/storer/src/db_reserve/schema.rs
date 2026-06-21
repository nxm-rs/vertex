//! Six-table reserve schema: `table!` definitions plus their compound-key and
//! value codecs.
//!
//! All compound keys are big-endian, so encoded byte order equals
//! `(field, field, ...)` lexicographic order. The orderings are pinned by the
//! tests at the foot of this module.

use alloy_primitives::{B256, keccak256};
use nectar_postage::Stamp;
use nectar_primitives::{Bin, ChunkAddress};
use serde::{Deserialize, Serialize};
use vertex_storage::{DatabaseError, Decode, Encode, table};

use vertex_swarm_primitives::BatchId;

// Refcounted content payload: `addr -> (refcnt, typed_bytes)`. One row per
// distinct address (not per entry); the body is shared by every stamped entry of
// that content. Uncompressed: chunk bodies are arbitrary/encrypted.
table!(pub(crate) Payload, "reserve_payload", ChunkAddress, PayloadValue, compressed = false);

// Per-stamped-entry primary index: `(po, batch, stampHash, addr) -> EntryValue`.
// One row per stamped entry; the reserve size is this table's count. Keyed
// proximity-major so the furthest entry (smallest po) is the first key and a
// bin's entries are contiguous.
table!(pub(crate) Entry, "reserve_entry", EntryKey, EntryValue, compressed = false);

// Batch grouping: `(batch, po, addr, stampHash) -> ()`. Batch-major then bin then
// address, so a prefix cursor over a batch yields its entries bin-ascending for
// batch eviction.
table!(pub(crate) BatchGroup, "reserve_batch_group", BatchGroupKey, (), compressed = false);

// Insertion-order replay: `(bin, binid) -> ReplayValue`. Append-only per-bin
// index a redistribution/sync consumer replays; keyed so a bin's rows are
// contiguous and ascending by sequence.
table!(pub(crate) Replay, "reserve_replay", ReplayKey, ReplayValue, compressed = false);

// Per-bin insertion cursor: `bin -> u64`. Highest sequence assigned in each bin;
// next insertion writes `cursor + 1`. Never rewound on eviction, so sequences are
// never reused (the guarantee a sync consumer's resume point depends on). The
// increment in `store::put` surfaces overflow as a hard error rather than
// wrapping.
table!(pub(crate) BinCounter, "reserve_bin_counter", BinKey, u64, compressed = false);

// Single-key metadata table; the only key is `EPOCH_KEY`.
table!(pub(crate) ReserveMetadata, "reserve_metadata", MetadataKey, u64, compressed = false);

/// Single-byte discriminant for [`ReserveMetadata`] rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct MetadataKey(pub u8);

pub(crate) const EPOCH_KEY: MetadataKey = MetadataKey(0x00);

impl Encode for MetadataKey {
    type Encoded = [u8; 1];

    fn encode(self) -> Self::Encoded {
        [self.0]
    }
}

impl Decode for MetadataKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 1] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(bytes[0]))
    }
}

// Stamp-index arbiter slot is the `StampIndexTable` handle owned by the postage
// crate. The reserve only controls *when* it is written: it runs `decide`
// arbitration inside its own atomic put so admission and the six-table write
// commit together.

/// Newtype key wrapping a [`Bin`] for [`BinCounter`] (orphan rule: [`Bin`] is a
/// foreign type, so codecs cannot be implemented on it directly).
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

/// Refcounted content payload value. `typed_bytes` is the type-tagged
/// [`AnyChunk`] encoding (no stamp), shared across referencing entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PayloadValue {
    /// Live entries referencing this content; the row is deleted at zero.
    pub(crate) refcnt: u64,
    pub(crate) typed_bytes: Vec<u8>,
}

/// Compound key `(po, batch, stampHash, addr)` for [`Entry`].
///
/// Big-endian `[po: 1][batch: 32][stampHash: 32][addr: 32]` (97 bytes),
/// proximity-major: the furthest entry (smallest po) is the table's first key.
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

/// Per-entry value: the bin sequence the entry landed at plus its exact admitting
/// stamp (canonical 113-byte encoding).
///
/// The stamp is stored per entry, not in the shared payload, because distinct
/// entries of the same content carry distinct stamps; this lets
/// [`get`](crate::DbReserve) reconstruct a stamped chunk and lets an inclusion
/// proof carry the precise stamp the slot was won with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EntryValue {
    pub(crate) bin: u8,
    pub(crate) binid: u64,
    pub(crate) stamp_bytes: Vec<u8>,
}

/// Compound key `(batch, po, addr, stampHash)` for [`BatchGroup`].
///
/// Big-endian `[batch: 32][po: 1][addr: 32][stampHash: 32]` (97 bytes): a batch's
/// entries are a contiguous prefix ascending by bin.
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
/// Big-endian `[bin: 1][binid: 8]`: a bin's rows are contiguous and ascending by
/// sequence, the order the bin scan walks.
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

/// Flat projection of the stamped entry at a `(bin, binid)`, including the chunk
/// type so a sampler resolves the CAC-beats-SOC tie without a body read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReplayValue {
    pub(crate) address: ChunkAddress,
    pub(crate) batch_id: BatchId,
    pub(crate) stamp_hash: B256,
    /// Chunk type id ([`ChunkTypeId::as_u8`]): 0 = content, 1 = single-owner.
    pub(crate) chunk_type: u8,
}

/// Stable hash of the exact stamp version that admitted an entry.
///
/// Keccak over the stamp's canonical 113-byte serialization, so a re-stamp under
/// a different batch/index/timestamp yields a different hash. Carried in the index
/// rows as a compact stand-in a consumer compares to detect a re-stamp.
pub(crate) fn stamp_hash(stamp: &Stamp) -> B256 {
    keccak256(stamp.to_bytes())
}

// Key-codec ordering tests (pure, no database).
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
        let k = EntryKey::new(
            7,
            BatchId::repeat_byte(0x11),
            B256::repeat_byte(0x22),
            ChunkAddress::from([0x33u8; 32]),
        );
        assert_eq!(EntryKey::decode(k.encode().as_ref()).unwrap(), k);

        // Smaller po sorts first regardless of trailing fields.
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
