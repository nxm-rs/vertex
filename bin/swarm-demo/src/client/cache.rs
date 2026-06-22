//! Session-local in-memory chunk store for the browser upload/download path.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use nectar_primitives::store::{ChunkStoreError, SyncChunkGet, SyncChunkHas, SyncChunkPut};
use nectar_primitives::{AnyChunk, ChunkAddress, DEFAULT_BODY_SIZE};

/// A cheaply-clonable, session-local in-memory chunk store (clones share the map).
#[derive(Clone, Default)]
pub struct MemoryCache {
    chunks: Rc<RefCell<HashMap<ChunkAddress, AnyChunk>>>,
}

impl MemoryCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of chunks currently held.
    pub fn len(&self) -> usize {
        self.chunks.borrow().len()
    }

    /// Insert a chunk (idempotent on address).
    pub fn insert(&self, chunk: AnyChunk) {
        self.chunks.borrow_mut().insert(*chunk.address(), chunk);
    }

    /// Fetch a chunk by address, if held.
    pub fn fetch(&self, address: &ChunkAddress) -> Option<AnyChunk> {
        self.chunks.borrow().get(address).cloned()
    }

    /// Visit every held chunk by reference.
    pub fn for_each(&self, mut f: impl FnMut(&AnyChunk)) {
        for chunk in self.chunks.borrow().values() {
            f(chunk);
        }
    }

    /// Clone all held chunks into an owned map (e.g. to seed the network getter).
    pub fn snapshot_map(&self) -> HashMap<ChunkAddress, AnyChunk> {
        self.chunks.borrow().clone()
    }
}

impl SyncChunkPut<DEFAULT_BODY_SIZE> for MemoryCache {
    type Error = ChunkStoreError;

    fn put(&self, chunk: AnyChunk) -> Result<(), Self::Error> {
        self.insert(chunk);
        Ok(())
    }
}

impl SyncChunkGet<DEFAULT_BODY_SIZE> for MemoryCache {
    type Error = ChunkStoreError;

    fn get(&self, address: &ChunkAddress) -> Result<AnyChunk, Self::Error> {
        self.fetch(address)
            .ok_or_else(|| ChunkStoreError::not_found(address))
    }
}

impl SyncChunkHas<DEFAULT_BODY_SIZE> for MemoryCache {
    fn has(&self, address: &ChunkAddress) -> bool {
        self.chunks.borrow().contains_key(address)
    }
}
