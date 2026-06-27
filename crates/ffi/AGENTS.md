# AGENTS: crates/ffi/

The native FFI surface. This crate (`vertex-ffi`) is the primary public API for embedding Vertex into a native host: Dart and Flutter, Swift, Kotlin and JNI, C++, and other native runtimes. It exposes an embeddable Swarm client that joins a network and uploads and downloads chunks.

Root-level rules in `/AGENTS.md` apply here too, plus `docs/agents/api-surface.md` (FFI is the primary surface) and `docs/agents/wasm.md` (native-only gating). The notes below are the area-specific overlay.

## Shape

- `src/api/`: the public surface. Every reachable item is a binding-generation candidate. `client.rs` holds `VertexClient` (the opaque handle), its build, upload, and download methods, and the pull-based streaming handles `VertexDownloadStream` / `VertexUploadStream` (opaque, driven by an async `next()` the host awaits one item at a time, which is what makes a slow host pause the network reads). The handles are thin native adapters over the transport-agnostic byte-bounded pipeline in `vertex-swarm-stream`; the same core backs the browser wasm adapter and the future gRPC chunk service, so backpressure and ordering live in one crate. `types.rs` holds the flat input and output shapes; `logging.rs` holds the host logging surface (`init_logging`, `LogLine`, `LogLevel`); `metrics.rs` holds the host metrics surface (`init_metrics`, `metrics_snapshot`, the snapshot shapes).
- `src/api/error.rs`: `FfiError`, a flat `thiserror` enum with `strum::IntoStaticStr`. It carries pre-formatted strings so a host never needs a vertex-internal error type. It lives inside `api` (with a `crate::error` re-export for Rust consumers) so the codegen sees it and surfaces it as a typed host exception (a sealed class on the Dart side).
- `src/frb_generated.rs`: the flutter_rust_bridge generated glue, committed as the codegen output so a plain `cargo build -p vertex-ffi` succeeds without the codegen binary (the generated code is plain Rust against the `flutter_rust_bridge` runtime). It defines the Dart-backed `StreamSink<T>` the logging surface references and the opaque-handle dispatch for the chunk streams. Bulk chunk transfer is pull-based (an opaque handle with an async `next()`), not a pushed `StreamSink`: a `StreamSink` is a non-blocking fire-and-forget post to the Dart port and applies no backpressure, so it is right for low-volume logs but wrong for memory-bounded bulk data.
- `bindings/`: the committed per-language codegen output. `bindings/dart` is a minimal Dart package (the codegen requires its Dart output inside one); a Dart host runs `dart run build_runner build` there to materialize the `.freezed.dart` parts. `bindings/c/vertex_ffi.h` is a stub unless full_dep mode is enabled (see `flutter_rust_bridge.yaml`).
- `flutter_rust_bridge.yaml`: codegen config. `src/api` is the input; the generated Rust and the per-language bindings are the output.
- `build.rs`: registers the `frb_expand` cfg the `#[frb]` macro emits, keeping the workspace `unexpected_cfgs` lint clean.

## Dos

- Keep the API thin. The crate is a boundary, not a place for logic. Build the client through the highest-level builder entry point (`vertex_swarm_builder::DefaultClientBuilder`); drive chunks through `SwarmChunkSender` and `SwarmChunkProvider`.
- FFI is a crate, not a feature (see the Feature and cfg contract in `/AGENTS.md`): it never enables the builder's `reserve`, so the storer cone stays out, which the cone guard asserts. It does pull `vertex-swarm-builder` and therefore `vertex-storage-redb`: the builder is the native full-stack entry point and the FFI client is native-only, so redb stays as the persistent-cache backend a native mobile embedder will want (the native analogue of the browser's IndexedDB cache). The lighter `vertex_swarm_node::ClientLauncher` path keeps the verifying, selector-aware chunk provider in the builder, so adopting it would mean moving those provider types first; not worth it for a redb trim.
- Reconstruct strong types (`StampedChunk`, `ChunkAddress`, `Stamp`) immediately on entry. Raw bytes and strings live only in the `api::types` shapes; never let them flow into internal logic.
- Generate the C ABI from the Rust `api` module via flutter_rust_bridge. There is no hand-maintained parallel C header.
- Annotate exported items with `#[frb(...)]` so codegen sees the right shape. `#[frb(opaque)]` for handles, `#[frb(non_opaque)]` for plain data, `#[frb(ignore)]` on private helper structs in `src/api` so the codegen does not generate glue for them.
- Spell `Result<T, FfiError>` in full in `pub fn` signatures under `src/api`. The codegen does not expand the generic `FfiResult` alias; an unexpanded alias degrades the error to an opaque handle instead of a typed host exception. The alias is fine in private helpers.
- Gate runtime-bearing dependencies (the native tokio runtime) to non-wasm targets. The browser path is wasm-bindgen, a separate surface.

## Donts

- No `serde_json`, `serde_yaml`, or any text-format serde backend. No `reqwest`, `axum`, `hyper`, or HTTP handler framework. This is the FFI cone; HTTP+JSON is forbidden here.
- Do not hand-edit `src/frb_generated.rs` beyond the placeholder. Edit `src/api` and regenerate.
- Do not move domain logic here. Chunk, stamp, and address primitives live in `nectar`; node assembly lives in `vertex-swarm-builder`.
- Do not block the calling thread on a runtime the host owns. The client owns its own native runtime and blocks on it internally.

## Logging

A node embedded through this crate logs through the `tracing` facade. Until a host installs a subscriber, every event hits the global no-op dispatcher and the host sees nothing. `api::logging::init_logging(level, sink)` installs a process-global subscriber that filters by `level` and forwards each surviving event to the host as a typed `LogLine` over a `StreamSink`.

- `level` is an `EnvFilter`-style directive parsed by `tracing_subscriber::EnvFilter`: a bare level (`"info"`, `"debug"`, `"trace"`, `"warn"`, `"error"`) sets the global maximum, and per-target directives (`"info,vertex_topology=debug"`) tune individual modules. An unparseable directive returns `FfiError::Logging`.
- `LogLine` is a flat struct (`timestamp_ms`, `level`, `target`, `message`, `fields`), not JSON. The host receives it as a Dart `Stream` (or the binding language's equivalent). `LogLevel` is a typed enum the host matches on instead of parsing a string. `fields` carries the event's key-value pairs flattened to strings, with the reserved `message` field hoisted out.
- The forwarding layer is a custom `tracing_subscriber::Layer` in `logging.rs`, not `vertex-observability`'s `VertexTracer`. The custom layer targets the `LogLine` shape directly and keeps the dependency surface to `tracing` plus `tracing-subscriber` (workspace features include `env-filter`). `vertex-observability` is intentionally not a dependency: pulling even its `subscriber` slice would add `eyre` for no gain here, and the cone guard (`just check-cone`) keeps the heavier server slices out regardless.
- `init_logging` is single-shot. `tracing` permits one global subscriber per process, so a second call returns `FfiError::LoggingAlreadyInitialized` without disturbing the installed subscriber (a `OnceLock` guard, never a panic).
- To strip logging at compile time, a host sets a `tracing` `release_max_level_*` feature in its own `Cargo.toml`. Cargo feature unification applies it to this crate's `tracing` dependency, so events below the chosen level are compiled out and `init_logging` forwards nothing below it.

## Metrics

A node embedded through this crate records against the `metrics` facade, which is a no-op until a recorder is installed. `api::metrics::init_metrics()` installs a process-global snapshot recorder; `api::metrics::metrics_snapshot()` reads the current state back as a typed `MetricsSnapshot`. The design note is `docs/observability/ffi-metrics.md`.

- The surface is pull-based and generic over metric names: three sorted vectors (`CounterValue`, `GaugeValue`, `HistogramValue`) of flat name, labels, value entries plus a timestamp. Not JSON, per `docs/agents/api-surface.md`. New instrumentation anywhere in the workspace flows through with no FFI change.
- The recorder is local to `api::metrics`, built on `metrics-util`'s `Registry` (the `registry` slice only, `default-features = false`) with bounded per-series storage: counters and gauges are atomics, histograms are a count plus sum summary, never a value-retaining bucket. Memory stays fixed even if the host never polls.
- `init_metrics` is single-shot (a `OnceLock` guard, same pattern as `init_logging`): a second call returns `FfiError::MetricsAlreadyInitialized`. Hosts call it before `VertexClient::build` so early activity is captured.
- Do not reach for the Prometheus recorder, an HTTP `/metrics` endpoint, or any push exporter here; the cone guard (`just check-cone`) asserts the exporter stack stays out. A native Rust host that wants Prometheus embeds via `vertex-swarm-builder` and installs `vertex-observability`'s recorder itself.

## Regenerating bindings

After changing anything under `src/api`, regenerate and commit the codegen output. From this directory:

```
nix-shell -p flutter_rust_bridge_codegen -p dart --run "flutter_rust_bridge_codegen generate --no-deps-check --no-build-runner"
```

`dart` is needed for formatting the Dart output; `--no-deps-check` skips the pubspec.lock check (the bindings package is committed without a lockfile); `--no-build-runner` skips the freezed build, which a consuming host runs itself. Keep the codegen and the `flutter_rust_bridge` runtime crate on the same version: the codegen's polisher pins the crate dependency (`=2.12.0`) to its own version, so a nixpkgs codegen bump shows up as a Cargo.toml diff and the pubspec pin in `bindings/dart` must follow. CI does not run the codegen; the committed output is the build input.

## Tests

- `cargo test -p vertex-ffi`. The unit tests cover the boundary reconstruction and identity-building helpers, the log event extraction, and the metrics snapshot extraction, all without standing up a network.
