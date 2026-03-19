//! Benchmarks for topology lock contention and deduplication patterns.

use std::collections::HashSet;
use std::sync::Arc;

use alloy_primitives::B256;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use vertex_swarm_peer_manager::ProximityIndex;
use vertex_swarm_primitives::OverlayAddress;

/// Generate deterministic overlay addresses for benchmarking.
fn make_overlays(count: usize) -> Vec<OverlayAddress> {
    (0..count)
        .map(|i| {
            let mut bytes = [0u8; 32];
            bytes[0..8].copy_from_slice(&(i as u64).to_le_bytes());
            OverlayAddress::from(B256::from(bytes))
        })
        .collect()
}

/// Benchmark HashSet deduplication vs Vec::contains.
fn bench_deduplication(c: &mut Criterion) {
    let mut group = c.benchmark_group("deduplication");

    for size in [100, 500, 1000, 5000].iter() {
        let existing: Vec<OverlayAddress> = make_overlays(*size);
        let new_candidates: Vec<OverlayAddress> = make_overlays(*size);

        // Vec::contains approach (O(n²))
        group.bench_with_input(
            BenchmarkId::new("vec_contains", size),
            size,
            |b, _| {
                b.iter(|| {
                    let mut result = existing.clone();
                    for candidate in &new_candidates {
                        if !result.contains(candidate) {
                            result.push(*candidate);
                        }
                    }
                    black_box(result)
                })
            },
        );

        // HashSet approach (O(n))
        group.bench_with_input(
            BenchmarkId::new("hashset_dedup", size),
            size,
            |b, _| {
                b.iter(|| {
                    let existing_set: HashSet<_> = existing.iter().copied().collect();
                    let mut result = existing.clone();
                    result.extend(new_candidates.iter().filter(|c| !existing_set.contains(c)));
                    black_box(result)
                })
            },
        );
    }

    group.finish();
}

/// Benchmark ProximityIndex operations.
fn bench_proximity_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("proximity_index");

    let local = OverlayAddress::from(B256::ZERO);

    for size in [100, 500, 1000, 5000].iter() {
        let index = ProximityIndex::new(local, 31, 0);
        let overlays = make_overlays(*size);

        // Populate the index
        for overlay in &overlays {
            let _ = index.add(*overlay);
        }

        // Benchmark all_peers() - now optimized with pre-allocation
        group.bench_with_input(
            BenchmarkId::new("all_peers", size),
            size,
            |b, _| {
                b.iter(|| black_box(index.all_peers()))
            },
        );

        // Benchmark iter_by_proximity() - uses cached sorted list
        group.bench_with_input(
            BenchmarkId::new("iter_by_proximity", size),
            size,
            |b, _| {
                b.iter(|| {
                    let items: Vec<_> = index.iter_by_proximity().collect();
                    black_box(items)
                })
            },
        );

        // Benchmark iter_by_proximity_desc()
        group.bench_with_input(
            BenchmarkId::new("iter_by_proximity_desc", size),
            size,
            |b, _| {
                b.iter(|| {
                    let items: Vec<_> = index.iter_by_proximity_desc().collect();
                    black_box(items)
                })
            },
        );

        // Benchmark bin_sizes()
        group.bench_with_input(
            BenchmarkId::new("bin_sizes", size),
            size,
            |b, _| {
                b.iter(|| black_box(index.bin_sizes()))
            },
        );
    }

    group.finish();
}

/// Benchmark lock snapshot pattern vs holding lock during iteration.
fn bench_lock_patterns(c: &mut Criterion) {
    use parking_lot::RwLock;
    use std::collections::HashMap;

    let mut group = c.benchmark_group("lock_patterns");

    for size in [100, 500, 1000].iter() {
        let overlays = make_overlays(*size);
        let map: Arc<RwLock<HashMap<OverlayAddress, u8>>> = Arc::new(RwLock::new(
            overlays.iter().map(|o| (*o, 0u8)).collect(),
        ));

        // Pattern 1: Snapshot keys then iterate (optimized)
        group.bench_with_input(
            BenchmarkId::new("snapshot_keys", size),
            size,
            |b, _| {
                b.iter(|| {
                    let keys: HashSet<OverlayAddress> = map.read().keys().copied().collect();
                    let mut count = 0;
                    for key in keys {
                        if key.as_slice()[0] > 128 {
                            count += 1;
                        }
                    }
                    black_box(count)
                })
            },
        );

        // Pattern 2: Hold read lock during iteration (original)
        group.bench_with_input(
            BenchmarkId::new("hold_lock", size),
            size,
            |b, _| {
                b.iter(|| {
                    let guard = map.read();
                    let mut count = 0;
                    for (key, _) in guard.iter() {
                        if key.as_slice()[0] > 128 {
                            count += 1;
                        }
                    }
                    black_box(count)
                })
            },
        );
    }

    group.finish();
}

/// Benchmark HashSet vs HashMap::values().any() for in-flight overlay lookup.
fn bench_in_flight_lookup(c: &mut Criterion) {
    use std::collections::HashMap;

    let mut group = c.benchmark_group("in_flight_lookup");

    #[derive(Clone)]
    struct InFlight {
        overlay: OverlayAddress,
    }

    for size in [8, 16, 32, 64].iter() {
        let overlays = make_overlays(*size);

        let map: HashMap<u64, InFlight> = overlays
            .iter()
            .enumerate()
            .map(|(i, o)| (i as u64, InFlight { overlay: *o }))
            .collect();

        let set: HashSet<OverlayAddress> = overlays.iter().copied().collect();

        let target = overlays[*size / 2]; // Target in the middle

        // HashMap::values().any() - O(n)
        group.bench_with_input(
            BenchmarkId::new("hashmap_values_any", size),
            size,
            |b, _| {
                b.iter(|| {
                    black_box(map.values().any(|v| v.overlay == target))
                })
            },
        );

        // HashSet::contains() - O(1)
        group.bench_with_input(
            BenchmarkId::new("hashset_contains", size),
            size,
            |b, _| {
                b.iter(|| {
                    black_box(set.contains(&target))
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_deduplication,
    bench_proximity_index,
    bench_lock_patterns,
    bench_in_flight_lookup,
);
criterion_main!(benches);
