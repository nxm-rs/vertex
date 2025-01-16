#![allow(unknown_lints, clippy::incompatible_msrv, missing_docs)]
#![feature(async_closure)]

use alloy_primitives::keccak256;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rand::prelude::*;
use swarm_primitives::bmt::reference::RefHasher;
use swarm_primitives::bmt::{Hasher, HasherBuilder};
use swarm_primitives::Address;
use swarm_primitives::BMT_BRANCHES;
use swarm_primitives::{distance, proximity};
use tokio::runtime::Builder;

pub fn primitives(c: &mut Criterion) {
    let mut g = c.benchmark_group("primitives");
    let mut rng = rand::thread_rng();
    let random_chunk: Vec<u8> = (0..4096).map(|_| rng.gen()).collect();

    //g.bench_function("chunk_address/4096", |b| {
    //    let chunk = Chunk::new(&mut random_chunk.clone(), None, Options::default(), None);
    //    b.iter(|| {
    //        black_box(chunk.address());
    //    });
    //});
    g.bench_function("hash_baseline", |b| {
        b.iter(|| {
            black_box(keccak256(&random_chunk));
        })
    });
    g.bench_function("bmt_nonconcurrent", |b| {
        let hasher: RefHasher<BMT_BRANCHES> = RefHasher::new();
        b.iter(|| {
            black_box(hasher.hash(&random_chunk));
        })
    });
    g.bench_function("bmt_concurrent", |b| {
        let rt = Builder::new_multi_thread()
            .worker_threads(16)
            .build()
            .unwrap();
        b.to_async(&rt).iter(|| async {
            let mut hasher: Hasher = HasherBuilder::default().build().unwrap();
            black_box(async || {
                let _ = hasher.write(&random_chunk).await;
                let mut res = [0u8; 32];
                let _ = hasher.hash(&mut res);
            });
        });
    });
    // Generate some random addresses
    let x = Address::random();
    let y = Address::random();
    let a = Address::random();
    g.bench_function("distance", |b| {
        b.iter(|| {
            black_box(distance::distance(&x, &y));
        })
    });
    g.bench_function("distance_cmp", |b| {
        b.iter(|| {
            black_box(distance::distance_cmp(&a, &x, &y));
        })
    });
    g.bench_function("proximity", |b| {
        b.iter(|| {
            black_box(proximity::proximity(x.as_ref(), y.as_ref()));
        })
    });
    g.finish();
}

criterion_group!(benches, primitives);
criterion_main!(benches);
