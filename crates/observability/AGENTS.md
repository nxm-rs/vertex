# AGENTS: crates/observability/ and crates/metrics/

Two crates split by weight. `vertex-metrics` is the leaf: guards, macros, label utilities, the `LabelValue` trait, and the histogram bucket presets. `vertex-observability` is the heavy infra: tracing subscriber, OTLP exporters, the Prometheus recorder, the metrics HTTP server, and profiling. It re-exports the leaf, so most consumers depend only on `vertex-observability`.

Global rules: see root `/AGENTS.md`. The notes below are the area-specific overlay.

## Feature split: orthogonal slices plus the `host` umbrella

`default = []` is the light surface and is load-bearing: the plain config structs (`StdoutConfig`, `OtlpConfig`, `OtlpLogsConfig`, `MetricsServerConfig`), `LogFormat`, and the `vertex-metrics` re-exports (recording macros, RAII guards, label utilities, `LabelValue`, the bucket presets, `HistogramBucketConfig`) all compile feature-free, including on `wasm32`. A config-only or wasm consumer names these types without pulling the heavy stack and never sets `default-features = false`.

The native slices:

- `subscriber` (`tracing-subscriber`, `eyre`): console/stdout layer, the `LogFormat -> layer` conversion, `VertexTracer`, `TracingGuard`, `build_and_init`. Gates `layers.rs`, `guard.rs`, `tracer.rs`.
- `otlp` (implies `subscriber`): OTLP trace and log export layers and the W3C `TraceContextPropagator` registration.
- `prometheus` (`metrics-exporter-prometheus`, `metrics-util`, `metrics-process`, `vertex-tasks`, `eyre`): the Prometheus recorder, `HistogramRegistry`, process and jemalloc hooks, the recorder upkeep task.
- `http-server` (implies `prometheus`; `axum`, `tower`, `tower-http`, `tokio`, `serde`): `MetricsServer` and its profiling endpoints. `axum -> tokio[net] -> mio` does not build for `wasm32`.
- `host` = `subscriber + otlp + prometheus + http-server`. `profiling` and `jemalloc` imply `host`; `tokio-console` implies `subscriber`.

The bucket presets and `HistogramBucketConfig` physically live in the leaf as `vertex_metrics::buckets`; `vertex-observability` re-exports them as `metrics::buckets` and at its crate root so the host-side recorder and node crates compile unchanged. Instrumented library crates (topology, the `/swarm/...` wire crates, `vertex-storage-redb`) depend on `vertex-metrics` only, never on `vertex-observability`, and declare their `HISTOGRAM_BUCKETS` against `vertex_metrics::buckets`.

Consumer enablement: `vertex-node-core` enables nothing (plain config structs only), `vertex-node-commands` enables `otlp` (it sets up `VertexTracer`), `vertex-node-builder` enables `http-server` (it installs the Prometheus recorder and metrics server), `bin/vertex` enables `host`.

## Dos

- New metric primitives (RAII guards, macros, label helpers) go in `vertex-metrics`. Heavy infra (subscriber layers, exporters, HTTP servers) goes in `vertex-observability`.
- Derive `strum::IntoStaticStr` on every label enum. `LabelValue` is what makes labels zero-allocation.
- Use the lazy macros (`lazy_counter!`, `lazy_gauge!`, `lazy_histogram!`) instead of `metrics::counter!` in hot paths; they handle registration ordering.
- Histograms must pick a documented bucket config (`DURATION_FINE`, `DURATION_NETWORK`, `DURATION_SECONDS`, `LOCK_CONTENTION`, `POLL_DURATION`, `CONNECTION_LIFETIME`). Do not invent new buckets without updating `HistogramBucketConfig`.
- Span boundaries follow `docs/observability/design.md`. Read it before adding a new top-level span.

## Donts

- Do not add tracing-subscriber or OTLP code to `vertex-metrics`. Its `unused_crate_dependencies` lint exists to keep it light.
- Do not bake metric strings inline. Use a label enum so cardinality is visible in one place.
- Do not call `metrics::counter!` with a dynamic string. Cardinality explosions are why `LabelValue` exists.
- Do not log at info level in hot paths: debug for per-message detail, info for state changes.
- Do not reach into the Prometheus recorder from a consumer crate. It is installed once via `install_prometheus_recorder`; consumers use the metrics facade.

## Tests and local stack

- `cargo test -p vertex-metrics -p vertex-observability` covers both crates.
- The Docker Compose stack in `observability/` (Prometheus, Tempo, Loki, Grafana, Promtail) is the local rig for manual verification. See `observability/README.md`.
- When adding a new metric, add a reference row to `docs/observability/profiling.md` or `docs/observability/helpers.md`.
