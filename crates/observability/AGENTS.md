# AGENTS: crates/observability/ and crates/metrics/

Two crates split by weight. `vertex-metrics` is the leaf with guards, macros, label utilities, and the `LabelValue` trait. `vertex-observability` is the heavy infrastructure: tracing subscriber, OTLP exporters, the Prometheus recorder, the metrics HTTP server, and profiling. The observability crate re-exports the leaf so most consumers only depend on `vertex-observability`.

Root-level rules in `/AGENTS.md` apply here too. The notes below are the area-specific overlay.

## Feature split: orthogonal slices plus the `host` umbrella

`vertex-observability` carries the native infrastructure behind four orthogonal features and a `host` umbrella that unions them. The plain config structs (`StdoutConfig`, `OtlpConfig`, `OtlpLogsConfig`, `MetricsServerConfig`) and the `LogFormat` enum are dependency-free data that compile with no features enabled, so a config-only or wasm consumer can name these types without pulling the heavy stack.

The slices:

- `subscriber` (`tracing-subscriber`, `eyre`): the console/stdout layer, the `LogFormat -> layer` conversion, `VertexTracer`, `TracingGuard`, and `build_and_init`. Gates `layers.rs`, `guard.rs`, `tracer.rs`.
- `otlp` (implies `subscriber`; OpenTelemetry SDK and exporters): the OTLP trace and log export layers and the W3C `TraceContextPropagator` registration.
- `prometheus` (`metrics-exporter-prometheus`, `metrics-util`, `metrics-process`, `vertex-tasks`, `eyre`): the Prometheus recorder, `HistogramRegistry`, process and jemalloc hooks, and the recorder upkeep task.
- `http-server` (implies `prometheus`; `axum`, `tower`, `tower-http`, `tokio`, `serde`): the `MetricsServer` and its profiling endpoints.
- `host` = `subscriber + otlp + prometheus + http-server`: the full native stack.

The `http-server` slice pulls `axum` -> `tokio[net]` -> `mio`, which does not build for `wasm32`. The crate defaults to no features (the light surface), so wasm-cone crates and embedders get only the platform-neutral surface for free: the `vertex-metrics` re-exports (recording macros, RAII guards, label utilities, `LabelValue`) and the histogram bucket presets plus `HistogramBucketConfig`. The bucket presets and `HistogramBucketConfig` physically live in the `vertex-metrics` leaf as `vertex_metrics::buckets`; `vertex-observability` re-exports them as `metrics::buckets` and at its crate root so the host-side recorder and node crates compile unchanged. Instrumented library crates (topology, the `/swarm/...` wire crates, and `vertex-storage-redb`) depend on `vertex-metrics` only and never on `vertex-observability`.

The four remaining `vertex-observability` consumers each enable the minimal slice they use: `vertex-node-core` enables nothing (it names only the plain config structs), `vertex-node-commands` enables `otlp` (it sets up `VertexTracer`), `vertex-node-builder` enables `http-server` (it installs the Prometheus recorder and the metrics server), and `bin/vertex` enables `host` (the full stack). `profiling` and `jemalloc` imply `host`; `tokio-console` implies `subscriber`. `default = []` keeps the crate light by default (library-first), so embedders and the wasm cone never have to set `default-features = false`; native consumers opt into the slice they need as listed above. A wasm wire crate declares its `HISTOGRAM_BUCKETS` against `vertex_metrics::buckets` (slice-free) at the leaf.

## Dos

- New metric primitives (RAII guards, macros, label helpers) go in `vertex-metrics`. Heavy infra (subscriber layers, exporters, HTTP servers) goes in `vertex-observability`.
- Derive `strum::IntoStaticStr` on every label enum. The `LabelValue` trait is what makes labels zero-allocation.
- Use the lazy macros (`lazy_counter!`, etc.) instead of `metrics::counter!` in hot paths. The macros take care of registration ordering.
- Histograms must pick a documented bucket config (`DURATION_FINE`, `DURATION_NETWORK`, `DURATION_SECONDS`, `LOCK_CONTENTION`, `POLL_DURATION`, `CONNECTION_LIFETIME`). Do not invent new buckets without updating `HistogramBucketConfig`.
- Span boundaries follow the convention in `docs/observability/design.md`. Read it before adding a new top-level span.

## Donts

- Do not add tracing-subscriber or OTLP code to `vertex-metrics`. That crate has the `unused_crate_dependencies` lint precisely to keep it light.
- Do not bake metric strings inline. Use a label enum so the cardinality is visible in one place.
- Do not call `metrics::counter!` with a dynamic string. Cardinality explosions are the reason `LabelValue` exists.
- Do not log at info level in hot paths. The convention is debug for per-message detail, info for state changes.
- Do not reach into the Prometheus recorder from a consumer crate. The recorder is installed once via `install_prometheus_recorder` and consumers use the metrics crate facade.

## Tests and local stack

- `cargo test -p vertex-metrics -p vertex-observability` covers both crates.
- The Docker Compose stack in `observability/` (Prometheus, Tempo, Loki, Grafana, Promtail) is the local rig for manual verification. See `observability/README.md`.
- When adding a new metric, add a reference row to `docs/observability/profiling.md` or `docs/observability/helpers.md`.
