//! IndexedDB-mirroring cache backend for the browser client.
//!
//! A resident [`LruBackend`] serves reads and bounds memory; every mutation is
//! also written through to an [`IndexedDbDatabase`] so the cache survives a page
//! reload. Persistence is best-effort (the database mirror is fire-and-forget),
//! so a failed mirror write only costs a re-fetch, never correctness. The
//! resident copy is authoritative for serving.

use std::sync::Arc;

use nectar_postage::STAMP_SIZE;
use nectar_primitives::{AnyChunk, ChunkAddress, ContentChunk, SingleOwnerChunk};
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Decode, Encode, table};
use vertex_storage_indexeddb::IndexedDbDatabase;
use vertex_swarm_primitives::{CachedChunk, Stamp};

use super::{CacheBackend, LruBackend};
use crate::chunk_store::CacheValue;

/// IndexedDB object store name for the cached-chunk table.
pub(crate) const CACHE_STORE: &str = "chunk_cache";

/// A 32-byte chunk address used as the persisted table key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
struct AddrKey([u8; 32]);

impl From<&ChunkAddress> for AddrKey {
    fn from(addr: &ChunkAddress) -> Self {
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(addr.as_slice());
        Self(bytes)
    }
}

impl Encode for AddrKey {
    type Encoded = [u8; 32];
    fn encode(self) -> Self::Encoded {
        self.0
    }
}

impl Decode for AddrKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 32] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(bytes))
    }
}

// Values are the self-encoded `CacheValue` blob; postcard wraps the byte vec and
// the resident LRU already bounds size, so the persisted copy stays uncompressed.
table!(
    CacheTable,
    "chunk_cache",
    AddrKey,
    Vec<u8>,
    compressed = false
);

/// Mirror tag: which chunk variant the persisted blob holds.
const TAG_CONTENT: u8 = 0;
const TAG_SINGLE_OWNER: u8 = 1;

/// Blob layout `[tag][has_stamp][stamp? 113B][chunk wire bytes]`.
///
/// The fixed-size stamp leads the variable-length wire bytes so the split is
/// unambiguous: stamp presence is an explicit flag, never inferred from length
/// (a stampless content chunk's wire bytes routinely exceed the 113-byte stamp).
fn encode_value(value: &CacheValue) -> Vec<u8> {
    let (chunk, stamp) = (value.0.chunk(), value.0.stamp());
    let tag = if chunk.is_single_owner() {
        TAG_SINGLE_OWNER
    } else {
        TAG_CONTENT
    };
    let wire = chunk.clone().into_bytes();
    let mut out = Vec::with_capacity(2 + stamp.map_or(0, |_| STAMP_SIZE) + wire.len());
    out.push(tag);
    out.push(u8::from(stamp.is_some()));
    if let Some(stamp) = stamp {
        out.extend_from_slice(&stamp.to_bytes());
    }
    out.extend_from_slice(&wire);
    out
}

/// Decode a blob produced by [`encode_value`], or `None` on any malformed input.
fn decode_value(blob: &[u8]) -> Option<CacheValue> {
    let (&tag, rest) = blob.split_first()?;
    let (&has_stamp, rest) = rest.split_first()?;

    let (stamp, wire) = if has_stamp == 1 {
        if rest.len() < STAMP_SIZE {
            return None;
        }
        let (s, w) = rest.split_at(STAMP_SIZE);
        let bytes: [u8; STAMP_SIZE] = s.try_into().ok()?;
        (Some(Stamp::from_bytes(&bytes).ok()?), w)
    } else {
        (None, rest)
    };

    let chunk: AnyChunk = match tag {
        TAG_CONTENT => ContentChunk::try_from(wire).ok()?.into(),
        TAG_SINGLE_OWNER => SingleOwnerChunk::try_from(wire).ok()?.into(),
        _ => return None,
    };
    Some(CacheValue(CachedChunk::new(chunk, stamp)))
}

/// A resident LRU mirrored to IndexedDB.
pub struct IndexedDbBackend {
    resident: LruBackend,
    db: Arc<IndexedDbDatabase>,
}

impl IndexedDbBackend {
    /// The object store name the backing database must create.
    #[must_use]
    pub const fn store_name() -> &'static str {
        CACHE_STORE
    }

    /// Build a backend over an open database, loading any persisted chunks into
    /// the resident LRU. Entries that fail to decode are skipped.
    #[must_use]
    pub fn new(db: Arc<IndexedDbDatabase>, max_bytes: usize) -> Self {
        let resident = LruBackend::with_budget(max_bytes);
        if let Ok(entries) = db.view(|tx| tx.entries::<CacheTable>()) {
            for (key, blob) in entries {
                if let Some(value) = decode_value(&blob) {
                    resident.insert(ChunkAddress::from(key.0), value);
                }
            }
        }
        Self { resident, db }
    }
}

impl CacheBackend for IndexedDbBackend {
    fn insert(&self, address: ChunkAddress, value: CacheValue) {
        let blob = encode_value(&value);
        self.resident.insert(address, value);
        let key = AddrKey::from(&address);
        let _ = self.db.update(|tx| tx.put::<CacheTable>(key, blob));
    }

    fn get(&self, address: &ChunkAddress) -> Option<CacheValue> {
        self.resident.get(address)
    }

    fn contains(&self, address: &ChunkAddress) -> bool {
        self.resident.contains(address)
    }

    fn remove(&self, address: &ChunkAddress) {
        self.resident.remove(address);
        let key = AddrKey::from(address);
        let _ = self.db.update(|tx| tx.delete::<CacheTable>(key));
    }

    fn len(&self) -> usize {
        self.resident.len()
    }

    fn is_empty(&self) -> bool {
        self.resident.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use alloy_primitives::B256;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::{ContentChunk, SingleOwnerChunk};
    use vertex_swarm_primitives::Stamp;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn stamp_at(timestamp: u64) -> Stamp {
        let sig = alloy_primitives::Signature::from_raw(&[1u8; 65]).expect("signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, timestamp, sig)
    }

    fn cache_value(chunk: AnyChunk, stamp: Option<Stamp>) -> CacheValue {
        CacheValue(CachedChunk::new(chunk, stamp))
    }

    #[wasm_bindgen_test]
    fn stampless_content_round_trips() {
        // A content chunk's wire bytes exceed the stamp size, so a length-based
        // stamp inference would corrupt it; the explicit flag must round-trip.
        let chunk: AnyChunk = ContentChunk::new(&[7u8; 512][..])
            .expect("content chunk")
            .into();
        let value = cache_value(chunk, None);
        let decoded = decode_value(&encode_value(&value)).expect("decode");
        assert_eq!(decoded.0, value.0);
        assert!(decoded.0.stamp().is_none());
        assert!(decoded.0.chunk().is_content());
    }

    #[wasm_bindgen_test]
    fn stamped_single_owner_round_trips() {
        let signer = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x11)).expect("signer");
        let chunk: AnyChunk = SingleOwnerChunk::new(B256::repeat_byte(0x22), &b"soc"[..], &signer)
            .expect("soc")
            .into();
        let value = cache_value(chunk, Some(stamp_at(42)));
        let decoded = decode_value(&encode_value(&value)).expect("decode");
        assert_eq!(decoded.0, value.0);
        assert_eq!(decoded.0.stamp().expect("stamp").timestamp(), 42);
        assert!(decoded.0.chunk().is_single_owner());
    }

    #[wasm_bindgen_test]
    fn malformed_blob_is_none() {
        assert!(decode_value(&[]).is_none());
        // tag=content, has_stamp=1, but no stamp bytes follow.
        assert!(decode_value(&[TAG_CONTENT, 1]).is_none());
    }
}
