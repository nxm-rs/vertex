//! Criterion benchmarks for the redistribution-game primitives.
//!
//! These benchmarks measure the pure-compute primitives in
//! [`vertex_swarm_redistribution`]: the per-chunk anchor-keyed transformed
//! address, the reserve-sample selection, the proof-of-entitlement build, and
//! the committed-depth neighbourhood filter.
//!
//! The four measured operations are:
//!
//! - [`AnyChunk::transformed_address`]: the per-chunk anchor-keyed consensus
//!   hash (a prefix-BMT over a 4 KiB body), measured both for a single chunk and
//!   batched over a full candidate set (hash all N, then sort) at 1k / 10k
//!   chunks. Reported in [`Throughput::Bytes`] over the hashed body bytes so the
//!   figure converts directly to an aggregate MB/s.
//! - [`reserve_sample`]: the min-16 selection over a candidate set that mirrors
//!   a reserve neighbourhood (1k / 10k / 65k chunks).
//! - [`make_inclusion_proofs`]: the proof-of-entitlement build over a 16-item
//!   sample.
//! - [`canonical_neighbourhood`]: the committed-depth membership filter over a
//!   parameterised address set.
//!
//! Inputs are derived from a deterministic counter-based generator rather than a
//! random source, so repeated runs use identical inputs and the measurement is
//! reproducible. All chunk and sample fixtures are built with the typed nectar
//! constructors *outside* the measured region.

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

use vertex_swarm_redistribution::{
    ClaimAnchor, CommittedDepth, SAMPLE_SIZE, SampleAnchor, SampleItem, canonical_neighbourhood,
    make_inclusion_proofs, reserve_sample,
};

/// The raw bytes of the sample-time anchor, a fixed 32-byte value so the
/// transformed-address work is deterministic across runs.
const ANCHOR_BYTES: &[u8] = b"swarm-test-anchor-deterministic!";

/// The sample-time anchor as the typed [`SampleAnchor`].
fn sample_anchor() -> SampleAnchor {
    SampleAnchor::new(B256::from_slice(ANCHOR_BYTES))
}

/// A fixed claim-time anchor (value 30) that drives the witness indices in
/// [`make_inclusion_proofs`].
fn claim_anchor() -> ClaimAnchor {
    ClaimAnchor::new(B256::left_padding_from(&[30]))
}

/// Deterministic 256-bit value generator (SplitMix64 expanded into 32 bytes).
///
/// Reproducible across runs so the benchmark inputs never change. Not a CSPRNG;
/// it only needs to spread addresses across the keyspace so the depth filter
/// does representative work.
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

/// A deterministic pool of `count` distinct chunk addresses.
fn address_pool(count: usize) -> Vec<ChunkAddress> {
    let mut state = 0x0DDB_1A5E_5EED_0042u64;
    (0..count)
        .map(|_| SwarmAddress::from(next_b256(&mut state)))
        .collect()
}

/// A deterministic 4096-byte (`DEFAULT_BODY_SIZE`) CAC body seeded by `n`.
///
/// Each body is distinct so transformed addresses spread across the keyspace.
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

/// A typed CAC chunk over a deterministic full-size body.
///
/// `new` BMT-hashes the content and wraps it with its span, exactly the typed
/// constructor used by the conformance tests.
fn cac_chunk(n: u64) -> DefaultAnyChunk {
    DefaultContentChunk::new(cac_body(n))
        .map(DefaultAnyChunk::from)
        .expect("CAC chunk builds")
}

/// A typed single-owner chunk wrapping a deterministic full-size body.
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

/// A reserve-shaped pool of `count` sample items over distinct CAC bodies.
///
/// Each item carries a real typed CAC and its transformed address, exactly the
/// work `reserve_sample` orders by. Built outside the measured region.
fn sample_pool(count: usize) -> Vec<SampleItem> {
    let sample = sample_anchor();
    (0..count as u64)
        .map(|n| SampleItem::new(sample, cac_chunk(n)))
        .collect()
}

/// The per-chunk consensus hash: an anchor-keyed prefix-BMT over a 4 KiB body,
/// for both a CAC and a SOC. Reported in `Throughput::Bytes` over the 4096-byte
/// body so the figure converts to MB/s.
///
/// The `batch/{1000,10000}` cases hash the transformed address of every chunk in
/// a full candidate set and then sort the set by transformed address, reporting
/// bytes/s over all hashed bodies. This gives an aggregate MB/s at realistic
/// candidate-set sizes, directly comparable to an aggregate hash-and-sort figure
/// from another implementation. It is deliberately distinct from
/// [`reserve_sample`], which keeps only the smallest 16 via sorted insertion;
/// this case hashes and sorts the whole set.
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

    // Aggregate hash-and-sort over a full candidate set, for an MB/s figure at
    // realistic sample sizes. Chunks are built once outside the measured region.
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

/// The min-16 reserve-sample selection over candidate-set sizes that mirror a
/// reserve / neighbourhood. The per-item transformed-address work is realistic
/// (real 4 KiB CAC bodies), so this measures the genuine selection cost, not a
/// stripped-down sort.
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

/// The proof-of-entitlement build over a full 16-item sample. The sample is
/// selected once outside the measured region; only the RC chunk hash plus the
/// three witnesses (RC + OG + TR BMT proofs each) are timed.
fn bench_make_inclusion_proofs(c: &mut Criterion) {
    let sample = sample_anchor();
    let claim = claim_anchor();

    // A realistic 16-item sample drawn from a 1k reserve-shaped pool.
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

/// The committed-depth membership filter over a parameterised address set.
/// `depth_0_all` admits every address (pure filter cost); `depth_1_half` keeps
/// roughly half the keyspace.
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
