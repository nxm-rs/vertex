# FFI Metrics Snapshot

Design note for the embedded metrics surface in `vertex-ffi`: why it exists, which recorder backs it, what the snapshot looks like, and how a host is expected to poll it.

## Decision

Embedded hosts get a typed, pull-based snapshot of every metric the node records: `init_metrics()` installs a process-global recorder, `metrics_snapshot()` returns the current values as plain structs. The alternative (declaring embedded builds metrics-free) was rejected because the snapshot answers the support questions an embedding app actually gets ("is the node connected", "how many peers", "is it syncing") and its maintenance cost is near zero: the surface is generic over metric names, so new instrumentation anywhere in the workspace flows through with no FFI change and no binding regeneration.

## Who this is for

The snapshot serves hosts that reach the node only through the generated bindings (Dart and Flutter, Swift, Kotlin, C). The expected consumers, in order of importance:

1. An in-app diagnostics view. A mobile host polls while the view is visible and renders connectivity, depth, and traffic totals directly from the snapshot.
2. Host-owned telemetry. If the app ships analytics or crash context, it samples the snapshot and forwards selected series through its own pipeline, under its own consent and batching rules.

On mobile the host application, not the embedded library, owns exporting: a library that opens sockets to push telemetry on its own schedule fights battery budgets, user consent flows, and app-store review. The idiomatic split is exactly a pull surface: the library exposes data on demand and stays silent otherwise. That is why this is a snapshot call and not a stream, an exporter, or an HTTP endpoint.

A native Rust host (for example an e-reader application embedding the client through `vertex-swarm-builder`) does not use this surface at all. It records against the same `metrics` facade and installs whatever recorder it wants, including `vertex-observability`'s Prometheus recorder. The FFI snapshot exists only because a bindings-level host has no way to install a recorder of its own.

## Recorder

A small recorder local to `vertex-ffi` (`src/api/metrics.rs`), built on `metrics-util`'s sharded `Registry` with custom storage: counters and gauges are plain atomics, histograms are a bounded running summary (observation count plus value sum, two atomics). Rejected alternatives:

- `metrics-util`'s `DebuggingRecorder`: its histogram storage keeps every recorded value until drained, which is an unbounded allocation on a long-lived node whose host may never poll.
- The Prometheus recorder: it drags in the exporter stack the cone guard (`just check-cone`) exists to keep out, and renders text exposition where the bindings want typed structs.
- Any push exporter (OTLP): pushing inverts the ownership rule above.

Histograms deliberately carry no quantiles. Count and sum are memory-bounded no matter how rarely the host polls, and two consecutive snapshots give rates and interval averages, which covers the diagnostics use cases. Quantiles would need buckets or sketches, real memory and a bucket-configuration surface, for latency detail that belongs in the native observability stack. If a concrete host need appears, extending `HistogramValue` with fixed summary fields (min, max) is a compatible change.

## Snapshot shape

Generic, not per-metric: the snapshot is three vectors (counters, gauges, histograms) of flat entries carrying the metric name, its label pairs, and the value, plus a wall-clock timestamp. No JSON, per `docs/agents/api-surface.md`. Metric names and labels are the same ones documented in the metrics reference (`docs/observability/profiling.md`), without the `vertex_` exposition prefix the Prometheus exporter adds. Names are data, not API: the FFI contract is the entry shape, and hosts select series by name string the way a Grafana dashboard does.

## Polling

The host polls on demand. Each call walks every registered series (a few hundred at steady state, bounded-cardinality labels per the design rules) and allocates the returned vectors, so it is cheap but not free. Guidance for hosts: poll every 1 to 5 seconds while a diagnostics view is visible, stop polling when it is not, and never poll from a background task on mobile. `init_metrics()` should run before the client is built so early activity is captured; like logging initialization it is single-shot per process.
