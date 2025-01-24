use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rand::Rng;
use std::sync::Arc;
use tokio::runtime::Builder;

use swarm_primitives::bmt::{Hasher, Pool, PooledHasher, Prover};
use swarm_primitives_traits::{CHUNK_SIZE, SEGMENT_SIZE};

pub fn proofs(c: &mut Criterion) {
    let mut group = c.benchmark_group("proofs");
    let rt = Builder::new_multi_thread()
        .worker_threads(16)
        .build()
        .unwrap();

    // Precompute setup asynchronously
    let (root_hash, index, hasher, proof) = rt.block_on(async {
        let pool = Arc::new(Pool::new(1).await);
        let data: Vec<u8> = (0..CHUNK_SIZE).map(|_| rand::random::<u8>()).collect();

        let mut hasher = pool.get_hasher().await.unwrap();
        hasher.set_span(data.len() as u64);
        hasher.write(&data).await.unwrap();

        let mut root_hash = [0u8; SEGMENT_SIZE];
        hasher.hash(&mut root_hash);

        let index = rand::thread_rng().gen_range(0..CHUNK_SIZE / SEGMENT_SIZE);
        let proof = hasher.proof(index).expect("Failed to generate proof");

        (root_hash, index, hasher, proof)
    });

    // Benchmark proof generation
    group.bench_function("generate_proof", |b| {
        b.to_async(&rt).iter(|| async {
            black_box(hasher.proof(index).expect("Failed to generate proof"));
        });
    });

    // Benchmark proof verification
    group.bench_function("verify_proof", |b| {
        b.to_async(&rt).iter(|| async {
            let result = Hasher::verify(index, proof.clone()).expect("Failed to verify proof");
            assert_eq!(result, root_hash, "Verification failed for index {}", index);
        });
    });

    // Cleanup to avoid panic on drop (requires tokio reactor)
    rt.block_on(async {
        drop(hasher);
    });

    group.finish();
}

criterion_group!(benches, proofs);
criterion_main!(benches);
