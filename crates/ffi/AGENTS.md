# AGENTS: crates/ffi/

The native FFI surface. This crate (`vertex-ffi`) is the primary public API for embedding Vertex into a native host: Dart and Flutter, Swift, Kotlin and JNI, C++, and other native runtimes. It exposes an embeddable Swarm client that joins a network and uploads and downloads chunks.

Root-level rules in `/AGENTS.md` apply here too, plus `docs/agents/api-surface.md` (FFI is the primary surface) and `docs/agents/wasm.md` (native-only gating). The notes below are the area-specific overlay.

## Shape

- `src/api/`: the public surface. Every reachable item is a binding-generation candidate. `client.rs` holds `VertexClient` (the opaque handle) and its build, upload, and download methods; `types.rs` holds the flat input and output shapes.
- `src/error.rs`: `FfiError`, a flat `thiserror` enum with `strum::IntoStaticStr`. It carries pre-formatted strings so a host never needs a vertex-internal error type.
- `src/frb_generated.rs`: the flutter_rust_bridge generated glue. Committed as an empty placeholder so a plain `cargo build -p vertex-ffi` succeeds without the codegen binary. The codegen overwrites it.
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

## Regenerating bindings

Run the flutter_rust_bridge codegen against `flutter_rust_bridge.yaml` (the binary is `flutter_rust_bridge_codegen`; on this host reach it with `nix-shell -p flutter_rust_bridge_codegen --run "..."`). The crate compiles without this step, so CI does not run it.

## Tests

- `cargo test -p vertex-ffi`. The unit tests cover the boundary reconstruction and identity-building helpers without standing up a network.
