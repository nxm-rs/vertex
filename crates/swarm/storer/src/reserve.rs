//! Reserve capacity management.
//!
//! The [`Reserve`] tracks storage capacity and handles eviction
//! when the store is full.

use nectar_primitives::{ChunkAddress, ProximityOrder};
use parking_lot::RwLock;
use tracing::{debug, warn};
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_primitives::OverlayAddress;

use crate::{ChunkStore, StorerError, StorerResult};

/// Eviction strategy for the in-memory reserve when full.
///
/// [`DbReserve`](crate::DbReserve) ignores this; it evicts through the
/// proximity- and batch-scoped [`ReserveStore`](vertex_swarm_api::ReserveStore) primitives.
#[derive(Debug, Clone, Copy, Default)]
pub enum EvictionStrategy {
    /// Return an error when full instead of evicting.
    #[default]
    NoEviction,
    /// Evict the oldest chunk by iteration order.
    EvictOldest,
    /// Evict the chunk furthest from our overlay address.
    EvictFurthest,
}

/// Reserve capacity tracker with eviction.
pub struct Reserve {
    capacity: u64,
    count: RwLock<u64>,
    strategy: EvictionStrategy,
    /// Required by [`EvictionStrategy::EvictFurthest`]; `None` otherwise.
    overlay: Option<OverlayAddress>,
}

impl Reserve {
    pub fn new(capacity: u64) -> Self {
        Self {
            capacity,
            count: RwLock::new(0),
            strategy: EvictionStrategy::default(),
            overlay: None,
        }
    }

    pub fn with_strategy(capacity: u64, strategy: EvictionStrategy) -> Self {
        Self {
            capacity,
            count: RwLock::new(0),
            strategy,
            overlay: None,
        }
    }

    /// Set the overlay address from the node identity, enabling
    /// [`EvictionStrategy::EvictFurthest`].
    #[must_use]
    pub fn with_identity(mut self, identity: &impl SwarmIdentity) -> Self {
        self.overlay = Some(identity.overlay_address());
        self
    }

    pub fn overlay(&self) -> Option<OverlayAddress> {
        self.overlay
    }

    /// Seed the count from an existing store.
    pub fn initialize_from<S: ChunkStore>(&self, store: &S) -> StorerResult<()> {
        let count = store.count()?;
        *self.count.write() = count;
        debug!(count, capacity = self.capacity, "Reserve initialized");
        Ok(())
    }

    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    pub fn count(&self) -> u64 {
        *self.count.read()
    }

    pub fn set_count(&self, count: u64) {
        *self.count.write() = count;
    }

    pub fn available(&self) -> u64 {
        let count = *self.count.read();
        self.capacity.saturating_sub(count)
    }

    pub fn has_room(&self) -> bool {
        *self.count.read() < self.capacity
    }

    /// Utilization percentage (0-100).
    pub fn utilization(&self) -> u8 {
        let count = *self.count.read();
        if self.capacity == 0 {
            return 100;
        }
        ((count * 100) / self.capacity) as u8
    }

    /// Reserve space for a chunk, evicting per the configured strategy if full.
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
            EvictionStrategy::EvictFurthest => match self.overlay {
                Some(overlay) => self.evict_furthest(store, &overlay),
                None => {
                    // Furthest ranking is undefined without our overlay; fall back
                    // to oldest rather than evict on a meaningless metric.
                    warn!("EvictFurthest needs an overlay; falling back to EvictOldest");
                    self.evict_oldest(store)
                }
            },
        }
    }

    pub fn on_added(&self) {
        let mut count = self.count.write();
        *count += 1;
    }

    pub fn on_removed(&self) {
        let mut count = self.count.write();
        *count = count.saturating_sub(1);
    }

    /// Decrement the count by `n` after a batch or bin eviction.
    pub fn on_removed_n(&self, n: u64) {
        let mut count = self.count.write();
        *count = count.saturating_sub(n);
    }

    fn evict_oldest<S: ChunkStore>(&self, store: &S) -> StorerResult<()> {
        let mut to_evict = None;

        store.for_each(|addr| {
            to_evict = Some(*addr);
            false // stop after the first
        })?;

        if let Some(addr) = to_evict {
            debug!(%addr, "Evicting chunk");
            store.delete(&addr)?;
            self.on_removed();
            Ok(())
        } else {
            // Empty but reported full; should not happen.
            Err(StorerError::StorageFull {
                capacity: self.capacity,
                used: 0,
            })
        }
    }

    /// Evict the chunk with the lowest proximity order to `overlay`, i.e. the one
    /// furthest from us. Ties broken by iteration order.
    fn evict_furthest<S: ChunkStore>(
        &self,
        store: &S,
        overlay: &OverlayAddress,
    ) -> StorerResult<()> {
        let mut furthest: Option<(ProximityOrder, ChunkAddress)> = None;

        store.for_each(|addr| {
            let po = addr.proximity(overlay);
            if furthest.is_none_or(|(best, _)| po < best) {
                furthest = Some((po, *addr));
            }
            true
        })?;

        if let Some((_, addr)) = furthest {
            debug!(%addr, "Evicting furthest chunk");
            store.delete(&addr)?;
            self.on_removed();
            Ok(())
        } else {
            // Empty but reported full; should not happen.
            Err(StorerError::StorageFull {
                capacity: self.capacity,
                used: 0,
            })
        }
    }
}

/// Reserve statistics snapshot.
#[derive(Debug, Clone)]
pub struct ReserveStats {
    pub capacity: u64,
    pub count: u64,
    pub available: u64,
    pub utilization: u8,
}

impl Reserve {
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

    #[test]
    fn evict_furthest_drops_the_lowest_proximity_chunk() {
        use vertex_swarm_test_utils::MockIdentity;

        // Overlay is all-zero, so 0x00.. is closest and 0x80.. (top bit set) is
        // furthest. Furthest eviction must drop the 0x80 address and keep 0x00.
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve =
            Reserve::with_strategy(2, EvictionStrategy::EvictFurthest).with_identity(&identity);
        let store = MemoryChunkStore::new();

        let near = test_address(0x00);
        let far = test_address(0x80);
        store.put(&near, b"data").unwrap();
        store.put(&far, b"data").unwrap();
        reserve.on_added();
        reserve.on_added();

        assert!(!reserve.has_room());
        reserve.try_reserve(&store).unwrap();

        assert!(reserve.has_room());
        assert!(store.contains(&near).unwrap(), "closest chunk retained");
        assert!(!store.contains(&far).unwrap(), "furthest chunk evicted");
    }
}
