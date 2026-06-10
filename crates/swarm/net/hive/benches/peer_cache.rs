//! Concurrent throughput comparison of peer-cache locking strategies.
//!
//! Models the hive inbound validation access pattern: several gossip batches
//! validated in parallel on blocking threads, each record performing one
//! cache lookup. A hit clones the cached record out; a miss runs validation
//! outside any lock and then inserts. The expensive miss-path work (ECDSA
//! recovery) is identical for every strategy and is therefore excluded; the
//! benchmark isolates the cache itself.
//!
//! Strategies:
//!
//! - `single_mutex`: one mutex around an LRU cache (the previous layout).
//! - `rwlock_peek`: a read-write lock around an LRU cache; reads use `peek`,
//!   trading away the LRU refresh on hit for shared read locks.
//! - `sharded_mutex/N`: per-shard mutexes selected by the last overlay byte;
//!   `N = 8` mirrors `src/cache.rs`.
//! - `dashmap_aged`: a concurrent sharded map with a logical-clock stamp per
//!   entry and a scan-evict once over capacity.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::sync::atomic::{AtomicU64, Ordering};

use alloy_primitives::{Address, B256, Signature};
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use dashmap::DashMap;
use hashlink::LruCache;
use parking_lot::{Mutex, RwLock};
use vertex_swarm_peer::{Nonce, SwarmAddress, SwarmPeer, Timestamp};

/// Mirrors the capacity documented in `src/cache.rs`.
const CAPACITY: usize = 256;
/// Concurrent validation threads.
const THREADS: usize = 8;
/// Records per gossip batch (MAX_BATCH_SIZE).
const BATCH: usize = 30;
/// Batches each thread validates per measured iteration.
const BATCHES_PER_THREAD: usize = 100;

/// The cache operations the validation path needs.
trait Cache: Sync {
    fn get(&self, overlay: &SwarmAddress, signature: &Signature) -> Option<SwarmPeer>;
    fn insert(&self, overlay: SwarmAddress, peer: SwarmPeer);
}

/// One mutex around the whole LRU cache.
struct SingleMutex(Mutex<LruCache<SwarmAddress, SwarmPeer>>);

impl SingleMutex {
    fn new() -> Self {
        Self(Mutex::new(LruCache::new(CAPACITY)))
    }
}

impl Cache for SingleMutex {
    fn get(&self, overlay: &SwarmAddress, signature: &Signature) -> Option<SwarmPeer> {
        let mut guard = self.0.lock();
        guard
            .get(overlay)
            .filter(|cached| cached.signature() == signature)
            .cloned()
    }

    fn insert(&self, overlay: SwarmAddress, peer: SwarmPeer) {
        self.0.lock().insert(overlay, peer);
    }
}

/// Read-write lock; the read path peeks without refreshing LRU order.
struct RwLockPeek(RwLock<LruCache<SwarmAddress, SwarmPeer>>);

impl RwLockPeek {
    fn new() -> Self {
        Self(RwLock::new(LruCache::new(CAPACITY)))
    }
}

impl Cache for RwLockPeek {
    fn get(&self, overlay: &SwarmAddress, signature: &Signature) -> Option<SwarmPeer> {
        self.0
            .read()
            .peek(overlay)
            .filter(|cached| cached.signature() == signature)
            .cloned()
    }

    fn insert(&self, overlay: SwarmAddress, peer: SwarmPeer) {
        self.0.write().insert(overlay, peer);
    }
}

/// Per-shard mutexes selected by the last overlay byte (the `src/cache.rs`
/// layout, generalized over shard count).
struct ShardedMutex {
    shards: Vec<Mutex<LruCache<SwarmAddress, SwarmPeer>>>,
    mask: usize,
}

impl ShardedMutex {
    fn new(shard_count: usize) -> Self {
        assert!(shard_count.is_power_of_two());
        Self {
            shards: (0..shard_count)
                .map(|_| Mutex::new(LruCache::new(CAPACITY / shard_count)))
                .collect(),
            mask: shard_count - 1,
        }
    }

    fn shard(&self, overlay: &SwarmAddress) -> &Mutex<LruCache<SwarmAddress, SwarmPeer>> {
        &self.shards[usize::from(overlay.0[31]) & self.mask]
    }
}

impl Cache for ShardedMutex {
    fn get(&self, overlay: &SwarmAddress, signature: &Signature) -> Option<SwarmPeer> {
        let mut shard = self.shard(overlay).lock();
        shard
            .get(overlay)
            .filter(|cached| cached.signature() == signature)
            .cloned()
    }

    fn insert(&self, overlay: SwarmAddress, peer: SwarmPeer) {
        self.shard(&overlay).lock().insert(overlay, peer);
    }
}

struct AgedEntry {
    peer: SwarmPeer,
    last_used: AtomicU64,
}

/// Concurrent sharded map with timestamp aging and scan-eviction.
struct DashMapAged {
    map: DashMap<SwarmAddress, AgedEntry>,
    clock: AtomicU64,
}

impl DashMapAged {
    fn new() -> Self {
        Self {
            map: DashMap::with_capacity(CAPACITY + 1),
            clock: AtomicU64::new(0),
        }
    }
}

impl Cache for DashMapAged {
    fn get(&self, overlay: &SwarmAddress, signature: &Signature) -> Option<SwarmPeer> {
        let entry = self.map.get(overlay)?;
        if entry.peer.signature() != signature {
            return None;
        }
        entry.last_used.store(
            self.clock.fetch_add(1, Ordering::Relaxed),
            Ordering::Relaxed,
        );
        Some(entry.peer.clone())
    }

    fn insert(&self, overlay: SwarmAddress, peer: SwarmPeer) {
        let stamp = self.clock.fetch_add(1, Ordering::Relaxed);
        self.map.insert(
            overlay,
            AgedEntry {
                peer,
                last_used: AtomicU64::new(stamp),
            },
        );
        while self.map.len() > CAPACITY {
            let mut oldest: Option<(SwarmAddress, u64)> = None;
            for entry in self.map.iter() {
                let used = entry.value().last_used.load(Ordering::Relaxed);
                if oldest.is_none_or(|(_, o)| used < o) {
                    oldest = Some((*entry.key(), used));
                }
            }
            let Some((key, _)) = oldest else { break };
            self.map.remove(&key);
        }
    }
}

/// Deterministic overlay; the last byte spreads keys evenly across shards,
/// matching the uniform distribution of real (hash-output) overlays.
fn overlay(i: u64) -> SwarmAddress {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&i.to_le_bytes());
    bytes[31] = i as u8;
    SwarmAddress::from(B256::from(bytes))
}

fn signature() -> Signature {
    let mut raw = [2u8; 65];
    raw[64] = 0;
    Signature::from_raw(&raw).expect("valid signature")
}

fn peer(i: u64) -> SwarmPeer {
    SwarmPeer::from_parts(
        vec!["/ip4/127.0.0.1/tcp/1634".parse().expect("valid multiaddr")],
        signature(),
        overlay(i),
        Nonce::new([0u8; 32]),
        Timestamp::from_seconds(0),
        None,
        Address::ZERO,
    )
}

/// Run the validation-shaped workload on `cache` from `THREADS` threads.
///
/// `miss_every` of 0 means a pure-hit workload; otherwise every
/// `miss_every`-th record in a batch is a fresh overlay (cache miss followed
/// by an insert, as after a successful validation).
fn run_workload<C: Cache>(cache: &C, sig: &Signature, fresh: &AtomicU64, miss_every: usize) {
    std::thread::scope(|scope| {
        for t in 0..THREADS {
            scope.spawn(move || {
                let mut cursor = (t * 37) as u64;
                let mut hits = 0usize;
                for _ in 0..BATCHES_PER_THREAD {
                    for slot in 0..BATCH {
                        let miss = miss_every != 0 && slot % miss_every == 0;
                        let key = if miss {
                            // Fresh keys live outside the hot range.
                            (1 << 32) + fresh.fetch_add(1, Ordering::Relaxed)
                        } else {
                            cursor = (cursor + 1) % CAPACITY as u64;
                            cursor
                        };
                        let addr = overlay(key);
                        match cache.get(&addr, sig) {
                            Some(found) => {
                                hits += 1;
                                black_box(found);
                            }
                            // Validation (outside any lock) then insert.
                            None => cache.insert(addr, peer(key)),
                        }
                    }
                }
                black_box(hits);
            });
        }
    });
}

fn bench_strategy<C: Cache>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    miss_every: usize,
    cache: C,
) {
    // Pre-fill the hot set so hit-path behaviour dominates from the start.
    for i in 0..CAPACITY as u64 {
        cache.insert(overlay(i), peer(i));
    }
    let sig = signature();
    let fresh = AtomicU64::new(0);
    let label = if miss_every == 0 { "all_hits" } else { "mixed" };
    group.bench_function(BenchmarkId::new(name, label), |b| {
        b.iter(|| run_workload(&cache, &sig, &fresh, miss_every));
    });
}

fn bench_peer_cache(c: &mut Criterion) {
    for miss_every in [0usize, 10] {
        let label = if miss_every == 0 {
            "peer_cache_all_hits"
        } else {
            "peer_cache_mixed_hit_miss"
        };
        let mut group = c.benchmark_group(label);
        group.throughput(Throughput::Elements(
            (THREADS * BATCHES_PER_THREAD * BATCH) as u64,
        ));
        bench_strategy(&mut group, "single_mutex", miss_every, SingleMutex::new());
        bench_strategy(&mut group, "rwlock_peek", miss_every, RwLockPeek::new());
        for shards in [4usize, 8, 16] {
            bench_strategy(
                &mut group,
                &format!("sharded_mutex/{shards}"),
                miss_every,
                ShardedMutex::new(shards),
            );
        }
        bench_strategy(&mut group, "dashmap_aged", miss_every, DashMapAged::new());
        group.finish();
    }
}

criterion_group!(benches, bench_peer_cache);
criterion_main!(benches);
