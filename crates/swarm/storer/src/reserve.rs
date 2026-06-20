//! Reserve capacity management.
//!
//! The [`Reserve`] tracks storage capacity and handles eviction
//! when the store is full.

use parking_lot::RwLock;
use tracing::{debug, warn};
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_primitives::OverlayAddress;

use crate::{ChunkStore, StorerError, StorerResult};

/// Eviction strategy when reserve is full.
///
/// Note: [`DbReserve`](crate::DbReserve) does not consult this in-memory
/// strategy. Its capacity is managed by the caller through the proximity- and
/// batch-scoped eviction primitives on
/// [`ReserveStore`](vertex_swarm_api::ReserveStore)
/// (`evict_furthest`/`evict_from_bin`/`evict_batch`), which compact the on-disk
/// indexes atomically. This enum and [`Reserve::try_reserve`] remain for the
/// in-memory store paths and are superseded for the db reserve.
#[derive(Debug, Clone, Copy, Default)]
pub enum EvictionStrategy {
    /// Don't evict, return error when full.
    #[default]
    NoEviction,
    /// Evict oldest chunks (FIFO-like based on iteration order).
    EvictOldest,
    /// Evict chunks furthest from our address (requires overlay).
    EvictFurthest,
}

/// Reserve capacity tracker.
///
/// Manages the storage quota and handles eviction when needed.
pub struct Reserve {
    /// Maximum chunk capacity.
    capacity: u64,
    /// Current chunk count.
    count: RwLock<u64>,
    /// Eviction strategy.
    strategy: EvictionStrategy,
    /// Local overlay address, threaded from the node identity at construction.
    /// Required by [`EvictionStrategy::EvictFurthest`] to rank chunks by
    /// proximity; `None` for the proximity-agnostic strategies.
    overlay: Option<OverlayAddress>,
}

impl Reserve {
    /// Create a new reserve with the given capacity.
    pub fn new(capacity: u64) -> Self {
        Self {
            capacity,
            count: RwLock::new(0),
            strategy: EvictionStrategy::default(),
            overlay: None,
        }
    }

    /// Create a reserve with a specific eviction strategy.
    pub fn with_strategy(capacity: u64, strategy: EvictionStrategy) -> Self {
        Self {
            capacity,
            count: RwLock::new(0),
            strategy,
            overlay: None,
        }
    }

    /// Thread the node identity, enabling proximity-ranked eviction
    /// ([`EvictionStrategy::EvictFurthest`]). Stores the derived overlay address
    /// (the reserve only needs its own location in address space, not the
    /// identity's signing capability), matching how the rest of the node is
    /// wired off the identity.
    #[must_use]
    pub fn with_identity(mut self, identity: &impl SwarmIdentity) -> Self {
        self.overlay = Some(identity.overlay_address());
        self
    }

    /// The local overlay address, if set. Required for `EvictFurthest`.
    pub fn overlay(&self) -> Option<OverlayAddress> {
        self.overlay
    }

    /// Initialize count from an existing store.
    pub fn initialize_from<S: ChunkStore>(&self, store: &S) -> StorerResult<()> {
        let count = store.count()?;
        *self.count.write() = count;
        debug!(count, capacity = self.capacity, "Reserve initialized");
        Ok(())
    }

    /// Get the capacity.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Get the current count.
    pub fn count(&self) -> u64 {
        *self.count.read()
    }

    /// Set the current count directly.
    ///
    /// Used by the per-entry [`DbReserve`](crate::DbReserve) to seed the
    /// in-memory size from the persisted entry-table count at construction (the
    /// authoritative size is the entry count, not the address count).
    pub fn set_count(&self, count: u64) {
        *self.count.write() = count;
    }

    /// Get available space.
    pub fn available(&self) -> u64 {
        let count = *self.count.read();
        self.capacity.saturating_sub(count)
    }

    /// Check if there's room for a new chunk.
    pub fn has_room(&self) -> bool {
        *self.count.read() < self.capacity
    }

    /// Get utilization percentage (0-100).
    pub fn utilization(&self) -> u8 {
        let count = *self.count.read();
        if self.capacity == 0 {
            return 100;
        }
        ((count * 100) / self.capacity) as u8
    }

    /// Try to reserve space for a new chunk.
    ///
    /// Returns `Ok(())` if space is available, or attempts eviction
    /// based on the configured strategy.
    pub fn try_reserve<S: ChunkStore>(&self, store: &S) -> StorerResult<()> {
        if self.has_room() {
            return Ok(());
        }

        match self.strategy {
            EvictionStrategy::NoEviction => {
                let count = *self.count.read();
                Err(StorerError::StorageFull {
                    capacity: self.capacity,
                    used: count,
                })
            }
            EvictionStrategy::EvictOldest => self.evict_oldest(store),
            EvictionStrategy::EvictFurthest => {
                // For now, fall back to oldest
                // TODO: Implement furthest eviction with overlay address
                warn!("EvictFurthest not implemented, falling back to EvictOldest");
                self.evict_oldest(store)
            }
        }
    }

    /// Record that a chunk was added.
    pub fn on_added(&self) {
        let mut count = self.count.write();
        *count += 1;
    }

    /// Record that a chunk was removed.
    pub fn on_removed(&self) {
        let mut count = self.count.write();
        *count = count.saturating_sub(1);
    }

    /// Record that `n` chunks were removed at once (a batch/bin eviction).
    pub fn on_removed_n(&self, n: u64) {
        let mut count = self.count.write();
        *count = count.saturating_sub(n);
    }

    /// Evict the oldest chunk (first one encountered in iteration).
    fn evict_oldest<S: ChunkStore>(&self, store: &S) -> StorerResult<()> {
        let mut to_evict = None;

        store.for_each(|addr| {
            to_evict = Some(*addr);
            false // Stop after first
        })?;

        if let Some(addr) = to_evict {
            debug!(%addr, "Evicting chunk");
            store.delete(&addr)?;
            self.on_removed();
            Ok(())
        } else {
            // Store is empty but we're "full"? Shouldn't happen
            Err(StorerError::StorageFull {
                capacity: self.capacity,
                used: 0,
            })
        }
    }
}

/// Statistics about the reserve.
#[derive(Debug, Clone)]
pub struct ReserveStats {
    /// Total capacity in chunks.
    pub capacity: u64,
    /// Current chunk count.
    pub count: u64,
    /// Available space.
    pub available: u64,
    /// Utilization percentage.
    pub utilization: u8,
}

impl Reserve {
    /// Get reserve statistics.
    pub fn stats(&self) -> ReserveStats {
        ReserveStats {
            capacity: self.capacity,
            count: self.count(),
            available: self.available(),
            utilization: self.utilization(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::memory::MemoryChunkStore;
    use nectar_primitives::ChunkAddress;

    fn test_address(n: u8) -> ChunkAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        ChunkAddress::new(bytes)
    }

    #[test]
    fn test_reserve_capacity() {
        let reserve = Reserve::new(100);
        assert_eq!(reserve.capacity(), 100);
        assert_eq!(reserve.count(), 0);
        assert_eq!(reserve.available(), 100);
        assert!(reserve.has_room());
    }

    #[test]
    fn identity_sets_the_overlay() {
        use vertex_swarm_test_utils::MockIdentity;

        let identity = MockIdentity::with_first_byte(0x42);
        let reserve =
            Reserve::with_strategy(2, EvictionStrategy::EvictFurthest).with_identity(&identity);
        assert_eq!(reserve.overlay(), Some(identity.overlay_address()));
        // The proximity-agnostic constructors leave it unset.
        assert_eq!(Reserve::new(2).overlay(), None);
    }

    #[test]
    fn test_reserve_tracking() {
        let reserve = Reserve::new(10);

        for _ in 0..5 {
            reserve.on_added();
        }

        assert_eq!(reserve.count(), 5);
        assert_eq!(reserve.available(), 5);
        assert_eq!(reserve.utilization(), 50);

        reserve.on_removed();
        assert_eq!(reserve.count(), 4);
    }

    #[test]
    fn test_reserve_full_no_eviction() {
        let reserve = Reserve::new(2);
        let store = MemoryChunkStore::new();

        store.put(&test_address(0), b"data").unwrap();
        store.put(&test_address(1), b"data").unwrap();
        reserve.on_added();
        reserve.on_added();

        assert!(!reserve.has_room());

        let result = reserve.try_reserve(&store);
        assert!(result.is_err());
    }

    #[test]
    fn test_reserve_eviction() {
        let reserve = Reserve::with_strategy(2, EvictionStrategy::EvictOldest);
        let store = MemoryChunkStore::new();

        store.put(&test_address(0), b"data").unwrap();
        store.put(&test_address(1), b"data").unwrap();
        reserve.on_added();
        reserve.on_added();

        assert!(!reserve.has_room());

        // Should evict one chunk
        reserve.try_reserve(&store).unwrap();
        assert!(reserve.has_room());
        assert_eq!(store.count().unwrap(), 1);
    }
}
