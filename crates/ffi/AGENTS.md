# AGENTS: crates/ffi/

`vertex-ffi`: the native FFI cdylib, primary public API for embedding a Vertex client into a native host (Dart and Flutter, Swift, Kotlin and JNI, C++). Joins a network, uploads and downloads chunks.

Global rules: root `/AGENTS.md`, plus `docs/agents/api-surface.md` (FFI is the primary surface) and `docs/agents/wasm.md` (native-only gating). Below is the area overlay.

## Shape

- `src/api/`: the public surface; every reachable item is a binding-generation candidate. `client.rs` holds `VertexClient` (opaque handle) with build/upload/download, plus pull-based streaming handles `VertexDownloadStream` / `VertexUploadStream` (async `next()`, one item at a time, so a slow host pauses network reads). The handles are thin native adapters over the byte-bounded pipeline in `vertex-swarm-stream`, shared with the wasm adapter and the gRPC chunk service; backpressure and ordering live in that one crate. `types.rs`: flat input/output shapes. `logging.rs` and `metrics.rs`: the host logging and metrics surfaces (see below).
- `src/api/error.rs`: `FfiError`, a flat `thiserror` enum with `strum::IntoStaticStr`, carrying pre-formatted strings so a host never needs a vertex-internal error type. It lives inside `api` (with a `crate::error` re-export) so codegen surfaces it as a typed host exception.
- `src/frb_generated.rs`: flutter_rust_bridge generated glue, committed so a plain `cargo build -p vertex-ffi` succeeds without the codegen binary. Bulk chunk transfer is pull-based (opaque handle, async `next()`), never a pushed `StreamSink`: `StreamSink` is fire-and-forget with no backpressure, fine for logs but wrong for memory-bounded bulk data.
- `bindings/`: committed per-language codegen output. `bindings/dart` is a minimal Dart package (codegen requires its output inside one); a host runs `dart run build_runner build` there for the `.freezed.dart` parts. `bindings/c/vertex_ffi.h` is a stub unless full_dep mode is enabled (see `flutter_rust_bridge.yaml`).
- `flutter_rust_bridge.yaml`: codegen config; `src/api` in, generated Rust and bindings out. `build.rs` registers the `frb_expand` cfg the `#[frb]` macro emits, keeping `unexpected_cfgs` clean.

## Dos

- Keep the API thin; the crate is a boundary, not a place for logic. Launch through the node-builder shell (`NodeBuilder::new()...launch_without_grpc()`), the binary's path minus the gRPC server; drive chunks through `SwarmChunkSender` and `SwarmChunkProvider`. The shell spawns the node task; the crate keeps the `TaskManager` alive and pulls the chunk client from `handle.components()`.
- FFI is a crate, not a feature (Feature and cfg contract in `/AGENTS.md`). Never enable the builder's `reserve`, so the storer cone stays out (cone guard asserts this). It does pull `vertex-swarm-builder` and therefore `vertex-storage-redb`, the persistent-cache backend a native mobile embedder wants. It pulls `vertex-node-builder` with `default-features = false`, so the Prometheus exporter and axum metrics server never enter this cone (cone guard keys off `metrics-exporter-prometheus`).
- Reconstruct strong types (`StampedChunk`, `ChunkAddress`, `Stamp`) immediately on entry. Raw bytes and strings live only in `api::types`; never let them flow into internal logic.
- Generate the C ABI from the Rust `api` module via flutter_rust_bridge; no hand-maintained parallel C header.
- Annotate exported items with `#[frb(...)]`: `opaque` for handles, `non_opaque` for plain data, `ignore` on private helper structs in `src/api`.
- Spell `Result<T, FfiError>` in full in `pub fn` signatures under `src/api`. Codegen does not expand the `FfiResult` alias; an unexpanded alias degrades the error to an opaque handle instead of a typed host exception. The alias is fine in private helpers.
- Gate runtime-bearing dependencies (the native tokio runtime) to non-wasm targets. The browser path is wasm-bindgen, a separate surface.

## Donts

- No `serde_json`, `serde_yaml`, or any text-format serde backend. No `reqwest`, `axum`, `hyper`, or HTTP handler framework. HTTP+JSON is forbidden in this cone.
- Do not hand-edit `src/frb_generated.rs`. Edit `src/api` and regenerate.
- Do not move domain logic here. Chunk, stamp, and address primitives live in `nectar`; node assembly in `vertex-swarm-builder`.
- Do not block the calling thread on a runtime the host owns. The client owns its own native runtime and blocks on it internally.

## Logging

The embedded node logs through the `tracing` facade, a no-op until a host installs a subscriber. `api::logging::init_logging(level, sink)` installs a process-global subscriber filtered by `level`, forwarding each surviving event as a typed `LogLine` over a `StreamSink`.

- `level` is parsed by `tracing_subscriber::EnvFilter` (bare level or per-target directives); an unparseable directive returns `FfiError::Logging`.
- `LogLine` is a flat struct, not JSON; `LogLevel` a typed enum the host matches on.
- The forwarding layer is a custom `tracing_subscriber::Layer` in `logging.rs`, not `vertex-observability`'s `VertexTracer`, keeping the dependency surface to `tracing` plus `tracing-subscriber` (`env-filter`). Do not depend on `vertex-observability` (its `subscriber` slice adds `eyre` for no gain; the cone guard keeps server slices out regardless).
- `init_logging` is single-shot (a `OnceLock` guard, never a panic): a second call returns `FfiError::LoggingAlreadyInitialized` without disturbing the installed subscriber.
- To strip logging at compile time, a host sets a `tracing` `release_max_level_*` feature; feature unification applies it here.

## Metrics

The embedded node records against the `metrics` facade, a no-op until a recorder is installed. `init_metrics()` installs a process-global snapshot recorder; `metrics_snapshot()` reads it back as a typed `MetricsSnapshot`. Design note: `docs/observability/ffi-metrics.md`.

- Pull-based and generic over metric names (sorted counter/gauge/histogram vectors plus a timestamp, not JSON). New instrumentation anywhere in the workspace flows through with no FFI change.
- The recorder is local to `api::metrics`, on `metrics-util`'s `Registry` (`registry` slice, `default-features = false`) with bounded per-series storage, so memory stays fixed even if the host never polls.
- `init_metrics` is single-shot (`OnceLock`, same as `init_logging`): a second call returns `FfiError::MetricsAlreadyInitialized`. Hosts call it before `VertexClient::build` so early activity is captured.
- No Prometheus recorder, HTTP `/metrics` endpoint, or push exporter here; the cone guard asserts the exporter stack stays out. A native Rust host wanting Prometheus embeds via `vertex-swarm-builder` and installs `vertex-observability`'s recorder itself.

## Regenerating bindings

After changing anything under `src/api`, regenerate and commit the output. From this directory:

```
nix-shell -p flutter_rust_bridge_codegen -p dart --run "flutter_rust_bridge_codegen generate --no-deps-check --no-build-runner"
```

Keep the codegen and the `flutter_rust_bridge` runtime crate on the same version: the polisher pins the crate dependency (`=2.12.0`), so a codegen bump is a Cargo.toml diff and the `bindings/dart` pubspec pin must follow. CI does not run the codegen; the committed output is the build input.

## Tests

- `cargo test -p vertex-ffi`. Unit tests cover boundary reconstruction and identity-building helpers, log event extraction, and metrics snapshot extraction, without standing up a network.
