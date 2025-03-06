use alloy_primitives::keccak256;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use digest::Digest;
use nectar_primitives_new::{bmt::BMTHasher, chunk::ChunkAddress, constants::*};
use rand::prelude::*;

pub fn primitives(c: &mut Criterion) {
    let mut group = c.benchmark_group("primitives");
    let mut rng = rand::thread_rng();

    // Create random data for benchmarking
    let random_chunk: Vec<u8> = (0..MAX_CHUNK_SIZE).map(|_| rng.gen()).collect();

    // Benchmark baseline Keccak256 performance
    group.bench_function("hash_baseline", |b| {
        b.iter(|| {
            black_box(keccak256(&random_chunk));
        })
    });

    // Benchmark the BMT implementation (non-concurrent)
    group.bench_function("bmt_hash", |b| {
        b.iter(|| {
            let mut hasher = BMTHasher::new();
            hasher.set_span(random_chunk.len() as u64);
            black_box(hasher.hash_to_b256(&random_chunk));
        })
    });

    // Benchmark the BMT implementation with different data sizes
    let sizes = [128, 512, 1024, 2048, 4096];
    for size in sizes {
        let data = vec![0x42; size];
        group.bench_with_input(BenchmarkId::new("bmt_by_size", size), &size, |b, &size| {
            b.iter(|| {
                let mut hasher = BMTHasher::new();
                hasher.set_span(size as u64);
                black_box(hasher.hash_to_b256(&data));
            });
        });
    }

    // Benchmark chunk address generation
    group.bench_function("chunk_address", |b| {
        let bytes_data = bytes::Bytes::from(random_chunk.clone());
        b.iter(|| {
            let mut hasher = BMTHasher::new();
            hasher.set_span(bytes_data.len() as u64);
            black_box(hasher.chunk_address(&bytes_data).unwrap());
        })
    });

    // Benchmark address proximity calculation
    group.bench_function("address_proximity", |b| {
        // Generate two random addresses
        let addr1 = ChunkAddress::new([1u8; ADDRESS_SIZE]);
        let addr2 = ChunkAddress::new([2u8; ADDRESS_SIZE]);

        b.iter(|| {
            black_box(addr1.proximity(&addr2));
        })
    });

    // Benchmark chunking efficiency
    group.bench_function("digest_trait_methods", |b| {
        let data = &random_chunk;
        b.iter(|| {
            black_box(BMTHasher::digest(data));
        })
    });

    group.finish();
}

criterion_group!(benches, primitives);
criterion_main!(benches);
