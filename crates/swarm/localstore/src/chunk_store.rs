//! The client chunk cache: a [`SwarmLocalStore`] over a byte-bounded LRU.
//!
//! [`ChunkStore`] is the client cache. It is a thin typed wrapper over the
//! generic [`BoundedLruStore`] from `vertex-store`, keyed by [`ChunkAddress`]
//! and holding [`StampedChunk`] values. The generic store stays domain-agnostic;
//! the Swarm-specific freshness policy (content chunks served indefinitely,
//! single-owner chunks served only while their stamp timestamp is within the
//! configured TTL) lives here, at the one spot that is type-aware.
//!
//! # Freshness policy
//!
//! - **Content chunks (CAC)** are immutable: their address is the BMT hash of
//!   their content, so a cached copy is forever valid. They are served on a hit
//!   and never expire.
//! - **Single-owner chunks (SOC)** are mutable: the owner re-signs new content
//!   at the same address. Each stamping carries a fresh owner-signed timestamp,
//!   so a cached SOC is served only while `now - stamp.timestamp() < ttl`. Past
//!   the TTL the hit is treated as a miss so the retrieval forwards to fetch the
//!   latest revision. On insert, a SOC with a newer stamp timestamp replaces an
//!   older cached one (last-write-wins by timestamp), so an update refreshes the
//!   cache rather than being dropped behind a stale entry.

use nectar_primitives::ChunkAddress;
use vertex_store::{BoundedLruStore, ByteSized};
use vertex_swarm_api::{SwarmLocalStore, SwarmResult};
use vertex_swarm_primitives::StampedChunk;

/// A reader of the current wall-clock time in nanoseconds since the Unix epoch.
///
/// Abstracted so the SOC freshness check is testable with a fixed clock. The
/// default reads the platform clock through `vertex-util-runtime`, which is
/// wasm-safe (browser clock on wasm32).
pub trait Clock: Send + Sync {
    /// Nanoseconds since the Unix epoch.
    fn now_ns(&self) -> i64;
}

/// The default clock, reading the platform wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ns(&self) -> i64 {
        vertex_util_runtime::time::now_unix_nanos()
    }
}

/// Newtype carrying a [`StampedChunk`] into the byte-bounded LRU.
///
/// The orphan rule forbids implementing the foreign [`ByteSized`] trait for the
/// foreign [`StampedChunk`] directly, so the cache value is this local wrapper.
/// Its byte size is the chunk payload plus the fixed-size stamp, which is what
/// the budget accounts; the payload is refcounted `Bytes`, so cloning a cache
/// value is a refcount bump, not a copy.
#[derive(Debug, Clone)]
struct CacheValue(StampedChunk);

impl ByteSized for CacheValue {
    fn byte_size(&self) -> usize {
        // Chunk payload size plus the fixed 113-byte stamp.
        self.0.chunk().size() + nectar_postage::STAMP_SIZE
    }
}

/// Client chunk cache: a [`SwarmLocalStore`] backed by a byte-bounded LRU.
///
/// Lossy by design: inserting past the budget evicts least-recently-used
/// entries. The SOC freshness policy is applied on `get` (an expired SOC reads
/// as a miss) and on `put` (an older SOC does not overwrite a newer cached one).
pub struct ChunkStore<C = SystemClock> {
    inner: BoundedLruStore<ChunkAddress, CacheValue>,
    /// SOC serve TTL in nanoseconds, measured against the stamp timestamp.
    soc_cache_ttl: u64,
    clock: C,
}

impl ChunkStore<SystemClock> {
    /// Create a cache bounded to `max_bytes` of resident chunk bytes, serving
    /// single-owner chunks for `soc_cache_ttl` nanoseconds past their stamp
    /// timestamp.
    #[must_use]
    pub fn with_budget(max_bytes: usize, soc_cache_ttl: u64) -> Self {
        Self::with_budget_and_clock(max_bytes, soc_cache_ttl, SystemClock)
    }
}

impl<C: Clock> ChunkStore<C> {
    /// Create a cache with an explicit clock (for deterministic tests).
    #[must_use]
    pub fn with_budget_and_clock(max_bytes: usize, soc_cache_ttl: u64, clock: C) -> Self {
        Self {
            inner: BoundedLruStore::with_budget(max_bytes),
            soc_cache_ttl,
            clock,
        }
    }

    /// Whether a single-owner chunk stamped at `stamp_ns` is still fresh enough
    /// to serve. Content chunks never call this.
    fn soc_is_fresh(&self, stamp_ns: u64) -> bool {
        let now = self.clock.now_ns();
        // A negative clock (pre-epoch) or a future-dated stamp both make the
        // age non-positive, which is trivially within any TTL: serve it. Cap our
        // own freshness with the configured TTL regardless.
        let age_ns = (now - i64::try_from(stamp_ns).unwrap_or(i64::MAX)).max(0);
        (age_ns as u128) < (self.soc_cache_ttl as u128)
    }

    /// The number of resident entries (test and metrics aid).
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl<C: Clock> SwarmLocalStore for ChunkStore<C> {
    fn put(&self, chunk: StampedChunk) -> SwarmResult<()> {
        let address = *chunk.address();
        // Last-write-wins by timestamp for single-owner chunks: a forwarded SOC
        // older than the cached one for this address must not overwrite the
        // fresher copy. Content chunks are immutable, so any cached copy is
        // identical and a re-insert is a harmless recency touch.
        if chunk.chunk().is_single_owner()
            && let Some(existing) = self.inner.get(&address)
            && existing.0.chunk().is_single_owner()
            && existing.0.stamp().timestamp() >= chunk.stamp().timestamp()
        {
            return Ok(());
        }
        self.inner.insert(address, CacheValue(chunk));
        Ok(())
    }

    fn get(&self, address: &ChunkAddress) -> SwarmResult<Option<StampedChunk>> {
        let Some(value) = self.inner.get(address) else {
            return Ok(None);
        };
        let stamped = value.0;
        // Content chunks are served indefinitely; a single-owner chunk is served
        // only while fresh, otherwise it reads as a miss so the caller forwards.
        if stamped.chunk().is_single_owner() && !self.soc_is_fresh(stamped.stamp().timestamp()) {
            return Ok(None);
        }
        Ok(Some(stamped))
    }

    fn contains(&self, address: &ChunkAddress) -> bool {
        self.inner.contains(address)
    }

    fn remove(&self, address: &ChunkAddress) -> SwarmResult<()> {
        self.inner.remove(address);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_primitives::{B256, Signature};
    use alloy_signer_local::PrivateKeySigner;
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk, SingleOwnerChunk};
    use std::sync::atomic::{AtomicI64, Ordering};

    /// A clock returning whatever value the test last set.
    #[derive(Default)]
    struct FixedClock(AtomicI64);

    impl FixedClock {
        fn new(ns: i64) -> Self {
            Self(AtomicI64::new(ns))
        }
        fn set(&self, ns: i64) {
            self.0.store(ns, Ordering::SeqCst);
        }
    }

    impl Clock for FixedClock {
        fn now_ns(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    impl Clock for &FixedClock {
        fn now_ns(&self) -> i64 {
            (*self).now_ns()
        }
    }

    fn stamp_at(timestamp: u64) -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, timestamp, sig)
    }

    fn content(payload: &'static [u8]) -> StampedChunk {
        let chunk: AnyChunk = ContentChunk::new(payload)
            .expect("valid content chunk")
            .into();
        StampedChunk::new(chunk, stamp_at(0))
    }

    fn soc(payload: &'static [u8], stamp_ns: u64) -> StampedChunk {
        let signer = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x11)).expect("signer");
        let chunk: AnyChunk = SingleOwnerChunk::new(B256::repeat_byte(0x22), payload, &signer)
            .expect("valid soc")
            .into();
        StampedChunk::new(chunk, stamp_at(stamp_ns))
    }

    #[test]
    fn round_trips_a_content_chunk() {
        let store = ChunkStore::with_budget(1 << 20, 1_000);
        let chunk = content(b"round trip payload");
        let address = *chunk.address();
        store.put(chunk.clone()).unwrap();
        assert!(store.contains(&address));
        assert_eq!(store.get(&address).unwrap(), Some(chunk));
        store.remove(&address).unwrap();
        assert!(!store.contains(&address));
        assert_eq!(store.get(&address).unwrap(), None);
    }

    #[test]
    fn content_chunk_served_regardless_of_ttl() {
        let clock = FixedClock::new(0);
        let store = ChunkStore::with_budget_and_clock(1 << 20, 1, &clock);
        let chunk = content(b"immutable");
        let address = *chunk.address();
        store.put(chunk.clone()).unwrap();
        // Advance far past any TTL: a content chunk still serves.
        clock.set(1_000_000_000_000);
        assert_eq!(store.get(&address).unwrap(), Some(chunk));
    }

    #[test]
    fn fresh_soc_is_served() {
        let clock = FixedClock::new(1_000);
        let store = ChunkStore::with_budget_and_clock(1 << 20, 500, &clock);
        let chunk = soc(b"feed v1", 900);
        let address = *chunk.address();
        store.put(chunk.clone()).unwrap();
        // Age is 100ns, TTL is 500ns: still fresh.
        assert_eq!(store.get(&address).unwrap(), Some(chunk));
    }

    #[test]
    fn expired_soc_is_a_miss() {
        let clock = FixedClock::new(2_000);
        let store = ChunkStore::with_budget_and_clock(1 << 20, 500, &clock);
        let chunk = soc(b"feed v1", 900);
        let address = *chunk.address();
        store.put(chunk).unwrap();
        // Age is 1100ns, TTL is 500ns: expired, read as a miss so the caller
        // forwards. The entry is still resident (contains is true).
        assert_eq!(store.get(&address).unwrap(), None);
        assert!(store.contains(&address));
    }

    #[test]
    fn newer_soc_replaces_older_on_put() {
        let clock = FixedClock::new(1_000);
        let store = ChunkStore::with_budget_and_clock(1 << 20, 1_000_000, &clock);
        let old = soc(b"feed", 100);
        let new = soc(b"feed", 200);
        let address = *old.address();
        assert_eq!(address, *new.address(), "same owner+id is the same address");
        store.put(old).unwrap();
        store.put(new.clone()).unwrap();
        assert_eq!(
            store.get(&address).unwrap().unwrap().stamp().timestamp(),
            200
        );
    }

    #[test]
    fn older_soc_does_not_replace_newer_on_put() {
        let clock = FixedClock::new(1_000);
        let store = ChunkStore::with_budget_and_clock(1 << 20, 1_000_000, &clock);
        let new = soc(b"feed", 200);
        let old = soc(b"feed", 100);
        let address = *new.address();
        store.put(new).unwrap();
        store.put(old).unwrap();
        assert_eq!(
            store.get(&address).unwrap().unwrap().stamp().timestamp(),
            200,
            "an older SOC must not overwrite a newer cached one"
        );
    }
}
