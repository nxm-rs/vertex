# AGENTS.md - vertex-swarm-stream

Transport-agnostic, memory-bounded bulk get/put combinator over the Swarm chunk traits.

Global rules (terse rustdoc, no em-dashes, multiaddrs-not-underlay, no inline reference-impl notes) live in the root `/AGENTS.md`. The notes below are the area overlay.

## What lives here

`vertex-swarm-stream` turns the single-chunk [`SwarmChunkProvider`] and [`SwarmChunkSender`] entry points into completion-ordered [`Stream`]s over a whole address list (arrival order, never request order). The download pipeline yields `VerifiedChunk` items, keeping at most `StreamConfig::max_concurrency` retrievals in flight and admitting a new one only as the consumer drains a completed slot, so a slow consumer transitively pauses the network reads and the heap stays bounded regardless of list length. A `VerifiedChunk` is proven to answer the address that requested it and carries an `Option<Stamp>`: the stamp is optional because a delivery may omit it and address integrity is stamp-independent. The upload pipeline pushes a chunk iterator under the same concurrency cap. `MAX_CHUNK_BYTES` is the per-chunk ceiling.

## Ordering invariant

The get pipeline yields chunks in completion (arrival) order, never request order, and every item carries its own `ChunkAddress`. Any caller that needs file or byte order MUST reorder downstream; the nectar `WindowedReader` is the reference consumer and the gRPC adapter relies on it via `buffer_unordered`. No retrieval-side change may assume or reintroduce ordered delivery: doing so turns the concurrency knobs into correctness-affecting parameters. `StreamConfig::max_concurrency` is a pipeline-depth and memory ceiling (a bandwidth-delay-product bound), not a per-peer overrun guard; bounding concurrent substreams to one peer is a separate concern that lives in the node retrieval layer.

It depends only on the trait surface (`vertex-swarm-api`, `vertex-swarm-primitives`), `nectar-primitives`, `vertex-tasks` (the `MaybeSend*` aliases), and light async/error helpers (`futures`, `pin-project-lite`, `thiserror`). No node internal type, no libp2p, so it sits in the FFI cone and the wasm cone alike.

## Consumers

- The native FFI adapter in `vertex-ffi` (`src/api/client.rs`) wraps these streams.
- The browser adapter in `src/wasm.rs` (`cfg(target_arch = "wasm32")`) surfaces them to JavaScript as async iterators.
- The gRPC chunk service in `vertex-swarm-rpc` streams the same items over the wire.

All three share this core, so a backpressure or ordering fix lands once.

## Layout

- `src/lib.rs`: the core. `StreamConfig`, `get_stream`/`GetStream`, `get_stream_from`/`GetStreamFrom`, `put_stream`/`try_put_stream`/`PutStream`, `ChunkClientExt`, `MAX_CHUNK_BYTES`. Native and wasm both compile this.
- `src/wasm.rs`: `cfg(target_arch = "wasm32")` only. The wasm-bindgen adapter over trait-object provider/sender, exported as concrete JS types (wasm-bindgen cannot export generics).

## Rules

- The core stays transport-agnostic. No FFI, no JS, no libp2p, no storage backend types in `lib.rs`. New surface a non-node consumer would not want belongs elsewhere.
- The crate is in the wasm cone: it must compile for `wasm32-unknown-unknown`. Keep wasm-only deps (`wasm-bindgen`, `wasm-bindgen-futures`, `js-sys`) under the `cfg(target_arch = "wasm32")` target table.
- Tests are plain `#[tokio::test]` against in-memory providers.
