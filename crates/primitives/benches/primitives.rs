#![allow(unknown_lints, clippy::incompatible_msrv, missing_docs)]
#![feature(async_closure)]

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rand::prelude::*;
use std::sync::Arc;
use swarm_primitives::bmt::chunk::{Chunk, Options};
use swarm_primitives::bmt::pool::{Pool, PooledHasher};
use swarm_primitives::bmt::reference::RefHasher;
use swarm_primitives::bmt::HasherBuilder;
use swarm_primitives::Address;
use swarm_primitives::{distance, proximity};
use tokio::runtime::Builder;
use tokio::sync::Mutex;

pub fn primitives(c: &mut Criterion) {
    let mut g = c.benchmark_group("primitives");
    let mut rng = rand::thread_rng();
    let mut random_chunk: Vec<u8> = (0..4096).map(|_| rng.gen()).collect();

    g.bench_function("chunk_address/4096", |b| {
        let chunk = Chunk::new(&mut random_chunk.clone(), None, Options::default(), None);
        b.iter(|| {
            black_box(chunk.address());
        });
    });
    g.bench_function("bmt_nonconcurrent", |b| {
        let mut hasher = RefHasher::new(128);
        b.iter(|| {
            black_box(hasher.hash(&random_chunk));
        })
    });
    g.bench_function("bmt_concurrent", |b| {
        let random_chunk: Vec<u8> = (0..256).map(|_| rng.gen()).collect();
        let rt = Builder::new_multi_thread()
            .worker_threads(16)
            .build()
            .unwrap();
        b.to_async(&rt).iter(|| async {
            let mut hasher = HasherBuilder::default().build().unwrap();
            black_box(async || {
                let bytes_written = hasher.write(&random_chunk).await;
                let t = hasher.hash().await;
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
