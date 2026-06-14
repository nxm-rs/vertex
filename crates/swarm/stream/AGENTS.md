# AGENTS.md - vertex-swarm-stream

Transport-agnostic, memory-bounded bulk get/put combinator over the Swarm chunk traits.

## What lives here

`vertex-swarm-stream` is the shared core that turns the single-chunk [`SwarmChunkProvider`] and [`SwarmChunkSender`] entry points into ordered, byte-bounded [`Stream`]s over a whole address list. The download pipeline yields `VerifiedStampedChunk` items in input order, prefetching ahead of the consumer up to a window expressed in bytes and stopping the instant the consumer stops draining. The upload pipeline pushes a chunk iterator under the same byte budget. The bound lives in Rust: a slow consumer transitively pauses the network reads, so the heap stays flat at roughly `window_bytes` regardless of list length.

The crate depends only on `vertex-swarm-api` (the trait surface), `nectar-primitives`, `vertex-tasks`, and `futures`. It does not touch any node internal type and pulls no libp2p, so it is light enough to sit in the FFI cone and the wasm cone alike.

## Consumers

- The native FFI adapter in `vertex-ffi` wraps these streams as Dart sinks.
- The browser adapter in the `wasm` module (`src/wasm.rs`, `cfg(target_arch = "wasm32")`) surfaces them to JavaScript as async iterators.
- The future gRPC chunk service streams the same items over the wire.

Because all three share this core, a backpressure or ordering fix lands once.

## Layout

- `src/lib.rs`: the core. `StreamConfig`, `get_stream`/`GetStream`, `put_stream`/`try_put_stream`/`PutStream`, `MAX_CHUNK_BYTES`. Native and wasm both compile this.
- `src/wasm.rs`: `cfg(target_arch = "wasm32")` only. The wasm-bindgen adapter over trait-object provider/sender, exported as concrete JS types (wasm-bindgen cannot export generics).

## Rules

- The core stays transport-agnostic. No FFI, no JS, no libp2p, no storage backend types in `lib.rs`. New surface that a non-node consumer would not want belongs elsewhere.
- The crate is in the wasm cone: it must compile for `wasm32-unknown-unknown`. Keep wasm-only deps (`wasm-bindgen`, `wasm-bindgen-futures`, `js-sys`) under the `cfg(target_arch = "wasm32")` target table.
- Timers come from `vertex_tasks::time`, never `tokio::time`, so the wasm arm has a working clock.
- Tests are plain `#[tokio::test]` against in-memory providers; the wasm adapter is exercised with `wasm-bindgen-test`.
