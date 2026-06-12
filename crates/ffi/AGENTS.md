# AGENTS: crates/ffi/

The native FFI surface. This crate (`vertex-ffi`) is the primary public API for embedding Vertex into a native host: Dart and Flutter, Swift, Kotlin and JNI, C++, and other native runtimes. It exposes an embeddable Swarm client that joins a network and uploads and downloads chunks.

Root-level rules in `/AGENTS.md` apply here too, plus `docs/agents/api-surface.md` (FFI is the primary surface) and `docs/agents/wasm.md` (native-only gating). The notes below are the area-specific overlay.

## Shape

- `src/api/`: the public surface. Every reachable item is a binding-generation candidate. `client.rs` holds `VertexClient` (the opaque handle) and its build, upload, and download methods; `types.rs` holds the flat input and output shapes; `logging.rs` holds the host logging surface (`init_logging`, `LogLine`, `LogLevel`).
- `src/error.rs`: `FfiError`, a flat `thiserror` enum with `strum::IntoStaticStr`. It carries pre-formatted strings so a host never needs a vertex-internal error type.
- `src/frb_generated.rs`: the flutter_rust_bridge generated glue. Committed as a minimal placeholder so a plain `cargo build -p vertex-ffi` succeeds without the codegen binary. The codegen overwrites it. The placeholder also defines a stand-in `StreamSink<T>` (a no-op `add`) so the streaming API signatures in `logging.rs` compile before the codegen has run; the codegen replaces it with the real Dart-backed sink.
- `flutter_rust_bridge.yaml`: codegen config. `src/api` is the input; the generated Rust and the per-language bindings (Dart, C header) are the output.
- `build.rs`: registers the `frb_expand` cfg the `#[frb]` macro emits, keeping the workspace `unexpected_cfgs` lint clean.

## Dos

- Keep the API thin. The crate is a boundary, not a place for logic. Build the client through the highest-level builder entry point (`vertex_swarm_builder::DefaultClientBuilder`); drive chunks through `SwarmChunkSender` and `SwarmChunkProvider`.
- Reconstruct strong types (`StampedChunk`, `ChunkAddress`, `Stamp`) immediately on entry. Raw bytes and strings live only in the `api::types` shapes; never let them flow into internal logic.
- Generate the C ABI from the Rust `api` module via flutter_rust_bridge. There is no hand-maintained parallel C header.
- Annotate exported items with `#[frb(...)]` so codegen sees the right shape. `#[frb(opaque)]` for handles, `#[frb(non_opaque)]` for plain data.
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

## Metrics (not yet exposed)

Embedded metrics are not exposed yet. The `metrics` facade the node records against is a no-op without an installed recorder, and this crate installs none (the Prometheus recorder and its HTTP server live behind `vertex-observability`'s `prometheus`/`http-server` slices, which the cone guard keeps out of the FFI build). A typed, pull-based metrics snapshot (a plain struct the host reads on demand, not JSON, per `docs/agents/api-surface.md`) is a planned follow-up. Do not reach for the Prometheus recorder or an HTTP `/metrics` endpoint here.

## Regenerating bindings

Run the flutter_rust_bridge codegen against `flutter_rust_bridge.yaml` (the binary is `flutter_rust_bridge_codegen`; on this host reach it with `nix-shell -p flutter_rust_bridge_codegen --run "..."`). The crate compiles without this step, so CI does not run it. The codegen regenerates `src/frb_generated.rs` including the real `StreamSink<LogLine>` glue the `init_logging` stream needs on the Dart side.

## Tests

- `cargo test -p vertex-ffi`. The unit tests cover the boundary reconstruction and identity-building helpers without standing up a network.
