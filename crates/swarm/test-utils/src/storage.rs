//! Configurable storage and reserve mocks for behaviour tests.
//!
//! Two shapes cover the recurring stubs. [`MockStorage`] is a read-only reserve
//! snapshot for one bin: it serves a fixed set of pre-stamped chunks and the
//! per-bin cursor index the pullsync server reads, so its `put` is a no-op.
//! [`MockReserve`] is a mutable point store with a configurable responsibility
//! radius, for the ingest path that actually persists what it accepts.

use std::collections::HashMap;
use std::sync::Mutex;

use alloy_primitives::B256;
use nectar_primitives::{Bin, ChunkAddress, ProximityOrder};
use vertex_swarm_api::{
    BatchId, BinCursorStore, BinScanItem, PullStorage, ReserveStore, StampedChunk, StorageRadius,
    SwarmLocalStore, SwarmResult,
};
use vertex_swarm_primitives::CachedChunk;

/// A read-only reserve snapshot for a single bin.
///
/// Holds an ordered set of pre-stamped chunks plus their per-bin insertion
/// sequence, serving both the client-store read path and the pullsync server
/// snapshot. `put`/`remove` are no-ops: the snapshot is fixed at construction,
/// so a test drives it entirely through [`with_chunks`](Self::with_chunks).
#[derive(Default)]
pub struct MockStorage {
    bin: u8,
    epoch: u64,
    items: Vec<BinScanItem>,
    chunks: HashMap<ChunkAddress, StampedChunk>,
}

impl MockStorage {
    /// Snapshot `chunks` into `bin` under reserve `epoch`, assigning each a
    /// one-based insertion sequence in the given order.
    pub fn with_chunks(bin: Bin, epoch: u64, chunks: Vec<StampedChunk>) -> Self {
        let mut items = Vec::with_capacity(chunks.len());
        let mut index = HashMap::with_capacity(chunks.len());
        for (i, chunk) in chunks.into_iter().enumerate() {
            let address = *chunk.address();
            let stamp_hash = B256::from_slice(address.as_slice());
            items.push(BinScanItem {
                seq: i as u64 + 1,
                address,
                batch_id: BatchId::repeat_byte(0xbb),
                stamp_hash,
            });
            index.insert(address, chunk);
        }
        Self {
            bin: bin.get(),
            epoch,
            items,
            chunks: index,
        }
    }
}

impl SwarmLocalStore for MockStorage {
    fn put(&self, _chunk: CachedChunk) -> SwarmResult<()> {
        Ok(())
    }

    fn get(&self, address: &ChunkAddress) -> SwarmResult<Option<CachedChunk>> {
        Ok(self.chunks.get(address).cloned().map(CachedChunk::from))
    }

    fn contains(&self, address: &ChunkAddress) -> bool {
        self.chunks.contains_key(address)
    }

    fn remove(&self, _address: &ChunkAddress) -> SwarmResult<()> {
        Ok(())
    }
}

impl ReserveStore for MockStorage {
    fn storage_radius(&self) -> StorageRadius {
        StorageRadius::ZERO
    }

    fn is_responsible_for(&self, _address: &ChunkAddress) -> bool {
        true
    }

    fn count(&self) -> SwarmResult<u64> {
        Ok(self.items.len() as u64)
    }

    fn capacity(&self) -> u64 {
        u64::MAX
    }

    fn count_in(&self, _po: ProximityOrder) -> SwarmResult<u64> {
        Ok(0)
    }

    fn evict_furthest(&self) -> SwarmResult<Option<ChunkAddress>> {
        Ok(None)
    }

    fn evict_from_bin(&self, _bin: Bin, _max: u64) -> SwarmResult<u64> {
        Ok(0)
    }

    fn evict_batch(&self, _batch: BatchId, _up_to_bin: Option<Bin>, _max: u64) -> SwarmResult<u64> {
        Ok(0)
    }
}

impl BinCursorStore for MockStorage {
    fn bin_cursor(&self, bin: Bin) -> SwarmResult<u64> {
        if bin.get() == self.bin {
            Ok(self.items.last().map(|i| i.seq).unwrap_or(0))
        } else {
            Ok(0)
        }
    }

    fn scan_bin_from<'a>(
        &'a self,
        bin: Bin,
        start_seq: u64,
    ) -> SwarmResult<Box<dyn Iterator<Item = SwarmResult<BinScanItem>> + Send + 'a>> {
        let items: Vec<BinScanItem> = if bin.get() == self.bin {
            self.items
                .iter()
                .filter(|i| i.seq >= start_seq)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        Ok(Box::new(items.into_iter().map(Ok)))
    }
}

impl PullStorage for MockStorage {
    fn reserve_epoch(&self) -> u64 {
        self.epoch
    }
}

/// A mutable point store with a configurable responsibility radius.
///
/// Unlike [`MockStorage`], `put`/`remove` mutate the backing map, so the ingest
/// path can be asserted against what was stored. `is_responsible_for` and
/// `storage_radius` return the fixed values chosen at [`new`](Self::new),
/// letting a test exercise both the responsible and the forwarding branch.
pub struct MockReserve {
    chunks: Mutex<HashMap<ChunkAddress, CachedChunk>>,
    responsible: bool,
    radius: StorageRadius,
}

impl MockReserve {
    /// A reserve that reports `responsible` for every address and advertises
    /// `radius`.
    pub fn new(responsible: bool, radius: StorageRadius) -> Self {
        Self {
            chunks: Mutex::new(HashMap::new()),
            responsible,
            radius,
        }
    }
}

impl SwarmLocalStore for MockReserve {
    fn put(&self, chunk: CachedChunk) -> SwarmResult<()> {
        self.chunks.lock().unwrap().insert(*chunk.address(), chunk);
        Ok(())
    }

    fn get(&self, address: &ChunkAddress) -> SwarmResult<Option<CachedChunk>> {
        Ok(self.chunks.lock().unwrap().get(address).cloned())
    }

    fn contains(&self, address: &ChunkAddress) -> bool {
        self.chunks.lock().unwrap().contains_key(address)
    }

    fn remove(&self, address: &ChunkAddress) -> SwarmResult<()> {
        self.chunks.lock().unwrap().remove(address);
        Ok(())
    }
}

impl ReserveStore for MockReserve {
    fn storage_radius(&self) -> StorageRadius {
        self.radius
    }

    fn is_responsible_for(&self, _address: &ChunkAddress) -> bool {
        self.responsible
    }

    fn count(&self) -> SwarmResult<u64> {
        Ok(self.chunks.lock().unwrap().len() as u64)
    }

    fn capacity(&self) -> u64 {
        u64::MAX
    }

    fn count_in(&self, _po: ProximityOrder) -> SwarmResult<u64> {
        Ok(0)
    }

    fn evict_furthest(&self) -> SwarmResult<Option<ChunkAddress>> {
        Ok(None)
    }

    fn evict_from_bin(&self, _bin: Bin, _max: u64) -> SwarmResult<u64> {
        Ok(0)
    }

    fn evict_batch(&self, _batch: BatchId, _up_to_bin: Option<Bin>, _max: u64) -> SwarmResult<u64> {
        Ok(0)
    }
}
