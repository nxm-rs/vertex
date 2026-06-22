//! The storer retrieval-serve view: a forwarding cache layered over the reserve.
//!
//! Reads hit either backend, reserve first so an admission-validated in-AoR copy
//! wins on the rare overlap. Writes go to the cache only; reserve admission is the
//! reserve's own concern, reached through its own handle (pushsync ingest).

use std::sync::Arc;

use nectar_primitives::ChunkAddress;
use vertex_swarm_api::{SwarmLocalStore, SwarmResult};
use vertex_swarm_primitives::CachedChunk;

/// A forwarding cache layered over a reserve: reads from either, writes to the cache.
pub(crate) struct CacheThenReserve {
    /// Forwarding cache of out-of-AoR chunks; the write target.
    cache: Arc<dyn SwarmLocalStore>,
    /// Admission-validated in-AoR copies; read-only through this view.
    reserve: Arc<dyn SwarmLocalStore>,
}

impl CacheThenReserve {
    pub(crate) fn new(cache: Arc<dyn SwarmLocalStore>, reserve: Arc<dyn SwarmLocalStore>) -> Self {
        Self { cache, reserve }
    }
}

impl SwarmLocalStore for CacheThenReserve {
    fn put(&self, chunk: CachedChunk) -> SwarmResult<()> {
        self.cache.put(chunk)
    }

    fn get(&self, address: &ChunkAddress) -> SwarmResult<Option<CachedChunk>> {
        // Reserve first so an in-AoR copy wins over a cached one on overlap.
        if let Some(chunk) = self.reserve.get(address)? {
            return Ok(Some(chunk));
        }
        self.cache.get(address)
    }

    fn contains(&self, address: &ChunkAddress) -> bool {
        self.reserve.contains(address) || self.cache.contains(address)
    }

    fn remove(&self, address: &ChunkAddress) -> SwarmResult<()> {
        self.cache.remove(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_primitives::{B256, Signature};
    use alloy_signer_local::PrivateKeySigner;
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk, SingleOwnerChunk};
    use vertex_swarm_localstore::{ChunkStore, Clock};

    /// Fixed at the epoch so a stamped SOC stays within the cache TTL.
    struct EpochClock;

    impl Clock for EpochClock {
        fn now_ns(&self) -> i64 {
            0
        }
    }

    fn store() -> Arc<dyn SwarmLocalStore> {
        Arc::new(ChunkStore::with_budget_and_clock(
            1 << 20,
            1_000_000_000,
            EpochClock,
        ))
    }

    fn stamp_at(timestamp: u64) -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, timestamp, sig)
    }

    fn content(payload: &'static [u8]) -> CachedChunk {
        let chunk: AnyChunk = ContentChunk::new(payload)
            .expect("valid content chunk")
            .into();
        CachedChunk::new(chunk, None)
    }

    fn soc(id: u8, payload: &'static [u8], stamp_ns: u64) -> CachedChunk {
        let signer = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x11)).expect("signer");
        let chunk: AnyChunk = SingleOwnerChunk::new(B256::repeat_byte(id), payload, &signer)
            .expect("valid soc")
            .into();
        CachedChunk::new(chunk, Some(stamp_at(stamp_ns)))
    }

    #[test]
    fn serves_from_either_backend_reserve_first_on_overlap() {
        let cache = store();
        let reserve = store();

        let cache_only = content(b"only in the cache");
        let cache_addr = *cache_only.address();
        cache.put(cache_only.clone()).expect("cache put");

        let reserve_only = content(b"only in the reserve");
        let reserve_addr = *reserve_only.address();
        reserve.put(reserve_only.clone()).expect("reserve put");

        // Same address in both backends, reserve carrying the OLDER stamp, so a win
        // for the reserve copy can only be positional (reserve consulted first).
        let overlap_cache = soc(0x22, b"cache revision", 200);
        let overlap_reserve = soc(0x22, b"reserve revision", 100);
        let overlap_addr = *overlap_cache.address();
        assert_eq!(
            overlap_addr,
            *overlap_reserve.address(),
            "same owner+id is the same address"
        );
        cache.put(overlap_cache).expect("cache overlap put");
        reserve
            .put(overlap_reserve.clone())
            .expect("reserve overlap put");

        let composite = CacheThenReserve::new(Arc::clone(&cache), Arc::clone(&reserve));

        assert_eq!(
            composite.get(&cache_addr).expect("get cache-only"),
            Some(cache_only),
            "a chunk only in the cache is served"
        );
        assert!(composite.contains(&cache_addr));
        assert_eq!(
            composite.get(&reserve_addr).expect("get reserve-only"),
            Some(reserve_only),
            "a chunk only in the reserve is served"
        );
        assert!(composite.contains(&reserve_addr));

        assert_eq!(
            composite.get(&overlap_addr).expect("get overlap"),
            Some(overlap_reserve),
            "the reserve copy wins on an overlapping address regardless of stamp age"
        );
    }

    #[test]
    fn writes_route_to_the_cache_only() {
        let cache = store();
        let reserve = store();
        let composite = CacheThenReserve::new(Arc::clone(&cache), Arc::clone(&reserve));

        let chunk = content(b"forwarded out-of-aor chunk");
        let address = *chunk.address();
        composite.put(chunk.clone()).expect("composite put");

        assert!(
            cache.contains(&address),
            "the put lands in the forwarding cache"
        );
        assert!(
            !reserve.contains(&address),
            "the put must not reach the reserve"
        );

        // Remove through the composite must not touch the reserve.
        let reserved = content(b"admitted in-aor chunk");
        let reserved_addr = *reserved.address();
        reserve.put(reserved).expect("seed reserve");
        composite.remove(&reserved_addr).expect("composite remove");
        assert!(
            reserve.contains(&reserved_addr),
            "remove must not touch the reserve"
        );

        composite.remove(&address).expect("remove cached");
        assert!(!cache.contains(&address), "remove clears the cache entry");
    }
}
