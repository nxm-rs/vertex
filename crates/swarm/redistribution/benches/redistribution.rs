//! Criterion benchmarks for the redistribution-game compute primitives:
//! `transformed_address`, `reserve_sample`, `make_inclusion_proofs`, and
//! `canonical_neighbourhood`.
//!
//! Inputs come from a deterministic counter-based generator, so runs are
//! reproducible. Fixtures are built outside the measured region.

#![allow(
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "benchmark code over known-bounds, deterministic fixtures"
)]

use alloy_primitives::B256;
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use nectar_primitives::{
    ChunkAddress, DEFAULT_BODY_SIZE, DefaultAnyChunk, DefaultContentChunk, DefaultSingleOwnerChunk,
    SwarmAddress,
};

use vertex_swarm_postage::{BatchId, Stamp, StampIndex};
use vertex_swarm_redistribution::{
    ClaimAnchor, CommittedDepth, SAMPLE_SIZE, SampleAnchor, SampleItem, canonical_neighbourhood,
    make_inclusion_proofs, reserve_sample,
};

/// Fixed 32-byte sample-time anchor.
const ANCHOR_BYTES: &[u8] = b"swarm-test-anchor-deterministic!";

fn sample_anchor() -> SampleAnchor {
    SampleAnchor::new(B256::from_slice(ANCHOR_BYTES))
}

fn claim_anchor() -> ClaimAnchor {
    ClaimAnchor::new(B256::left_padding_from(&[30]))
}

/// Deterministic 256-bit generator (SplitMix64 expanded to 32 bytes). Not a
/// CSPRNG; just spreads addresses across the keyspace.
fn next_b256(state: &mut u64) -> B256 {
    let mut out = [0u8; 32];
    for chunk in out.chunks_mut(8) {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        chunk.copy_from_slice(&z.to_le_bytes());
    }
    B256::from(out)
}

fn address_pool(count: usize) -> Vec<ChunkAddress> {
    let mut state = 0x0DDB_1A5E_5EED_0042u64;
    (0..count)
        .map(|_| SwarmAddress::from(next_b256(&mut state)))
        .collect()
}

/// Deterministic `DEFAULT_BODY_SIZE` CAC body seeded by `n`, distinct per `n`.
fn cac_body(n: u64) -> Vec<u8> {
    let mut state = n.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    let mut body = vec![0u8; DEFAULT_BODY_SIZE];
    for word in body.chunks_mut(8) {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let bytes = z.to_le_bytes();
        word.copy_from_slice(&bytes[..word.len()]);
    }
    body
}

fn cac_chunk(n: u64) -> DefaultAnyChunk {
    DefaultContentChunk::new(cac_body(n))
        .map(DefaultAnyChunk::from)
        .expect("CAC chunk builds")
}

fn soc_chunk(n: u64) -> DefaultAnyChunk {
    let mut key = [0u8; 32];
    key[..8].copy_from_slice(&n.to_le_bytes());
    key[31] = 1; // keep the scalar non-zero / in range
    let signer = alloy_signer_local::PrivateKeySigner::from_slice(&key)
        .expect("deterministic signer key is valid");
    let mut id_state = n ^ 0xABCD_EF12;
    let id = next_b256(&mut id_state);
    DefaultSingleOwnerChunk::new(id, cac_body(n), &signer)
        .map(DefaultAnyChunk::from)
        .expect("SOC chunk builds")
}

/// Deterministic synthetic stamp for pool item `n`, with a distinct batch id.
///
/// Required because `make_inclusion_proofs` reads each slot's winning stamp; it
/// does not affect sample ordering or the BMT proofs.
fn fixture_stamp(n: u64) -> Stamp {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&n.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes());
    let batch: BatchId = B256::from(bytes);
    let index = StampIndex::new(n as u32, n as u32);
    let sig = alloy_primitives::Signature::test_signature();
    Stamp::with_index(batch, index, 1, sig)
}

/// Reserve-shaped pool of `count` sample items, each a real CAC with its
/// transformed address and a winning stamp.
fn sample_pool(count: usize) -> Vec<SampleItem> {
    let sample = sample_anchor();
    (0..count as u64)
        .map(|n| SampleItem::with_stamp(sample, cac_chunk(n), fixture_stamp(n)))
        .collect()
}

/// Per-chunk consensus hash (anchor-keyed prefix-BMT over the body), for a CAC
/// and a SOC, reported as MB/s. The `batch/{1000,10000}` cases hash every chunk
/// in a candidate set then sort by transformed address, for an aggregate MB/s.
fn bench_transformed_address(c: &mut Criterion) {
    let mut group = c.benchmark_group("transformed_address");

    group.throughput(Throughput::Bytes(DEFAULT_BODY_SIZE as u64));

    let cac = cac_chunk(1);
    group.bench_function(BenchmarkId::from_parameter("cac"), |b| {
        b.iter(|| {
            let tr = black_box(&cac).transformed_address(black_box(ANCHOR_BYTES));
            black_box(tr);
        });
    });

    let soc = soc_chunk(1);
    group.bench_function(BenchmarkId::from_parameter("soc"), |b| {
        b.iter(|| {
            let tr = black_box(&soc).transformed_address(black_box(ANCHOR_BYTES));
            black_box(tr);
        });
    });

    for &count in &[1_000usize, 10_000] {
        let chunks: Vec<DefaultAnyChunk> = (0..count as u64).map(cac_chunk).collect();
        group.throughput(Throughput::Bytes((count * DEFAULT_BODY_SIZE) as u64));
        group.bench_with_input(BenchmarkId::new("batch", count), &chunks, |b, chunks| {
            b.iter(|| {
                let mut addrs: Vec<_> = chunks
                    .iter()
                    .map(|ch| ch.transformed_address(ANCHOR_BYTES))
                    .collect();
                addrs.sort_unstable_by(|a, c| a.as_slice().cmp(c.as_slice()));
                black_box(&addrs);
            });
        });
    }

    group.finish();
}

/// The min-16 reserve-sample selection over neighbourhood-sized candidate sets,
/// with realistic per-item transformed-address work over real CAC bodies.
fn bench_reserve_sample(c: &mut Criterion) {
    let mut group = c.benchmark_group("reserve_sample");

    for &pool in &[1_024usize, 10_240, 65_536] {
        let candidates = sample_pool(pool);
        group.throughput(Throughput::Elements(pool as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(pool),
            &candidates,
            |b, candidates| {
                b.iter(|| {
                    let sample = reserve_sample(black_box(candidates).iter().cloned());
                    black_box(sample);
                });
            },
        );
    }

    group.finish();
}

/// The proof-of-entitlement build over a full 16-item sample (RC chunk hash plus
/// the RC + OG + TR BMT-proof witnesses); selection happens outside the timer.
fn bench_make_inclusion_proofs(c: &mut Criterion) {
    let sample = sample_anchor();
    let claim = claim_anchor();

    let items = reserve_sample(sample_pool(1_024));
    assert_eq!(
        items.len(),
        SAMPLE_SIZE,
        "sample must be full for the proof"
    );

    let mut group = c.benchmark_group("make_inclusion_proofs");
    group.throughput(Throughput::Elements(SAMPLE_SIZE as u64));
    group.bench_function("sample_16", |b| {
        b.iter(|| {
            let proofs =
                make_inclusion_proofs(black_box(&items), black_box(sample), black_box(claim))
                    .expect("inclusion proofs build");
            black_box(proofs);
        });
    });
    group.finish();
}

/// The committed-depth membership filter. `depth_0_all` admits every address;
/// `depth_1_half` keeps roughly half the keyspace.
fn bench_canonical_neighbourhood(c: &mut Criterion) {
    let anchor = SwarmAddress::zero();
    let depth_0 = CommittedDepth::ZERO;
    let depth_1 = CommittedDepth::try_from(1).expect("depth 1 is in range");
    let mut group = c.benchmark_group("canonical_neighbourhood");

    for &n in &[1_024usize, 16_384, 65_536] {
        let addrs = address_pool(n);
        group.throughput(Throughput::Elements(n as u64));

        group.bench_with_input(BenchmarkId::new("depth_0_all", n), &addrs, |b, addrs| {
            b.iter(|| {
                let hood = canonical_neighbourhood(
                    black_box(&anchor),
                    black_box(depth_0),
                    addrs.iter().copied(),
                );
                black_box(hood);
            });
        });

        group.bench_with_input(BenchmarkId::new("depth_1_half", n), &addrs, |b, addrs| {
            b.iter(|| {
                let hood = canonical_neighbourhood(
                    black_box(&anchor),
                    black_box(depth_1),
                    addrs.iter().copied(),
                );
                black_box(hood);
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_transformed_address,
    bench_reserve_sample,
    bench_make_inclusion_proofs,
    bench_canonical_neighbourhood
);
criterion_main!(benches);
