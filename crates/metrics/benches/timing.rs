//! Profiles the per-poll timing cost: uncached `histogram!` per call versus a
//! cached handle versus a sampled guard, plus a no-recorder floor.
//!
//! A hand-rolled summing recorder keeps histogram storage O(1); the debugging
//! recorder from `metrics-util` retains every recorded value unbounded.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use metrics::{
    Counter, Gauge, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};
use vertex_metrics::{TimingGuard, TimingSampler};

/// Summing histogram handle: counts records into one atomic, no retention.
#[derive(Default)]
struct SummingHistogram(AtomicU64);

impl HistogramFn for SummingHistogram {
    fn record(&self, _value: f64) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// Recorder returning one shared summing histogram for every registration.
struct SummingRecorder(Arc<SummingHistogram>);

impl Recorder for SummingRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn register_counter(&self, _: &Key, _: &Metadata<'_>) -> Counter {
        Counter::noop()
    }
    fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }
    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::from_arc(self.0.clone())
    }
}

static CACHED_HIST: LazyLock<Histogram> =
    LazyLock::new(|| metrics::histogram!("bench_poll_duration_seconds"));
static SAMPLER_HIST: LazyLock<Histogram> =
    LazyLock::new(|| metrics::histogram!("bench_poll_duration_seconds"));

const SAMPLE_INTERVAL: NonZeroU32 = NonZeroU32::new(64).expect("nonzero");

fn bench_timing(c: &mut Criterion) {
    let recorder = SummingRecorder(Arc::new(SummingHistogram::default()));
    let mut group = c.benchmark_group("poll_timing");

    // (a) Current pattern: register the histogram every call, guard per call.
    group.bench_function("uncached_guard", |b| {
        metrics::with_local_recorder(&recorder, || {
            b.iter(|| {
                let guard = TimingGuard::new(metrics::histogram!("bench_poll_duration_seconds"));
                black_box(&guard);
                drop(guard);
            });
        });
    });

    // (b) Cached handle: register once, guard per call.
    group.bench_function("cached_guard", |b| {
        metrics::with_local_recorder(&recorder, || {
            let _ = LazyLock::force(&CACHED_HIST);
            b.iter(|| {
                let guard = TimingGuard::new(CACHED_HIST.clone());
                black_box(&guard);
                drop(guard);
            });
        });
    });

    // (c) Sampled guard at interval 64: skip path is a decrement and branch.
    group.bench_function("sampler_64", |b| {
        metrics::with_local_recorder(&recorder, || {
            let mut sampler = TimingSampler::new(&SAMPLER_HIST, SAMPLE_INTERVAL);
            b.iter(|| {
                let guard = sampler.start();
                black_box(&guard);
            });
        });
    });

    // (d) No-recorder floor: global noop recorder, guard per call.
    group.bench_function("no_recorder", |b| {
        b.iter(|| {
            let guard = TimingGuard::new(metrics::histogram!("bench_poll_duration_seconds"));
            black_box(&guard);
            drop(guard);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_timing);
criterion_main!(benches);
