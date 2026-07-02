//! Allocation-diet benchmarks for the dispatch hot loop: candidate ordering
//! and the in-flight availability partition, the per-chunk CPU work of a bulk
//! download.

use std::num::NonZeroUsize;
use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use nectar_primitives::ChunkAddress;
use vertex_swarm_api::{Au, Ledger, SwarmPricing};
use vertex_swarm_node::{InflightLimit, PeerInflightLimiter, PeerScores, PeerSelector};
use vertex_swarm_primitives::OverlayAddress;

fn peer(n: usize) -> OverlayAddress {
    let mut bytes = [0u8; 32];
    bytes[0] = (n >> 8) as u8;
    bytes[1] = n as u8;
    bytes[2] = 0xa5;
    OverlayAddress::from(bytes)
}

/// True for every `nth` peer of the deterministic candidate mix.
fn mix(overlay: &OverlayAddress, nth: u8) -> bool {
    overlay
        .as_bytes()
        .get(1)
        .is_some_and(|b| b.is_multiple_of(nth))
}

/// Every 7th peer is warned; everyone else carries a healthy score.
struct MixScores;

impl PeerScores for MixScores {
    fn peer_score(&self, overlay: &OverlayAddress) -> Option<f64> {
        if mix(overlay, 7) {
            Some(-10.0)
        } else {
            Some(0.0)
        }
    }
}

/// At the unit price: every 8th peer refuses, every 5th settles-and-admits,
/// the rest admit with headroom.
struct MixLedger;

impl Ledger for MixLedger {
    fn balance(&self, _overlay: &OverlayAddress) -> Au {
        Au::ZERO
    }

    fn reserved(&self, _overlay: &OverlayAddress) -> Au {
        Au::ZERO
    }

    fn disconnect_line(&self, overlay: &OverlayAddress) -> Au {
        if mix(overlay, 8) {
            Au::ZERO
        } else {
            Au::from_amount(1000)
        }
    }

    fn settle_trigger(&self, overlay: &OverlayAddress) -> Au {
        if mix(overlay, 5) {
            Au::ZERO
        } else {
            Au::from_amount(1000)
        }
    }
}

struct UnitPricer;

impl SwarmPricing for UnitPricer {
    fn price(&self, _chunk: &ChunkAddress) -> Au {
        Au::from_amount(1)
    }

    fn peer_price(&self, _peer: &OverlayAddress, _chunk: &ChunkAddress) -> Au {
        Au::from_amount(1)
    }
}

fn candidates(n: usize) -> Vec<OverlayAddress> {
    (0..n).map(peer).collect()
}

fn bench_order(c: &mut Criterion) {
    let selector = PeerSelector::new(
        Arc::new(MixScores),
        Arc::new(MixLedger),
        Arc::new(UnitPricer),
    );
    let chunk = ChunkAddress::new([0xcc; 32]);

    for n in [32usize, 128] {
        let input = candidates(n);
        c.bench_function(&format!("selector_order_{n}"), |b| {
            b.iter_batched(
                || input.clone(),
                |cands| selector.order(cands, &chunk),
                BatchSize::SmallInput,
            )
        });
        c.bench_function(&format!("selector_order_closest_admissible_{n}"), |b| {
            b.iter_batched(
                || input.clone(),
                |cands| selector.order_closest_admissible(cands, &chunk),
                BatchSize::SmallInput,
            )
        });
    }
}

fn bench_available(c: &mut Criterion) {
    let limiter = PeerInflightLimiter::new(NonZeroUsize::MIN);
    let input = candidates(128);
    // Every 4th peer is at its cap, so the partition genuinely reorders.
    let _permits: Vec<_> = input
        .iter()
        .filter(|p| mix(p, 4))
        .filter_map(|p| limiter.try_acquire(p))
        .collect();

    c.bench_function("inflight_available_128", |b| {
        b.iter_batched(
            || input.clone(),
            |cands| InflightLimit::available(&limiter, cands),
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, bench_order, bench_available);
criterion_main!(benches);
