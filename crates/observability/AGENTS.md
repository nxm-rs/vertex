# AGENTS: crates/observability/ and crates/metrics/

Two crates split by weight. `vertex-metrics` is the leaf with guards, macros, label utilities, and the `LabelValue` trait. `vertex-observability` is the heavy infrastructure: tracing subscriber, OTLP exporters, the Prometheus recorder, the metrics HTTP server, and profiling. The observability crate re-exports the leaf so most consumers only depend on `vertex-observability`.

Root-level rules in `/AGENTS.md` apply here too. The notes below are the area-specific overlay.

## Feature split: `server` vs the wasm-safe core

`vertex-observability` carries the native infrastructure behind a default-on `server` feature: the tracing subscriber, OTLP exporters, the Prometheus recorder, the `axum` metrics HTTP server, the process hooks, and profiling. That stack pulls `axum` -> `tokio[net]` -> `mio`, which does not build for `wasm32`, so the workspace dependency sets `default-features = false`. With the server off, the crate still exposes the platform-neutral surface: the `vertex-metrics` re-exports (recording macros, RAII guards, label utilities, `LabelValue`) and the histogram bucket presets plus `HistogramBucketConfig`. The wasm cone (topology and the `/swarm/...` wire crates) takes this default. Native consumers that serve metrics or set up tracing (`vertex-node-builder`, `vertex-node-core`, `vertex-node-commands`, `bin/vertex`) re-enable it with `features = ["server"]`. `profiling`, `jemalloc`, and `tokio-console` all imply `server`. Keep `HistogramBucketConfig` and the bucket presets in `metrics::buckets` (server-free) so a wasm wire crate can still declare its `HISTOGRAM_BUCKETS`.

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
