## Wasm target guidance

Vertex aims to run a **client** node type in the browser (`wasm32-unknown-unknown`). This is a planning constraint that shapes every crate, not a future migration. Bootnode and storer node types are explicitly out of scope for wasm; they will only ever run natively.

### Why

A wasm client unlocks light-client embeddings in dapps, indexer UIs, and the Nexum gateway without a separate codebase. The Swarm domain logic and the higher-level protocol code can be the same in both worlds; only transport, runtime, storage, and observability differ.

### Status

- Already no_std capable with a `default = ["std"]` feature: `vertex-swarm-primitives`, `vertex-swarm-spec`, `vertex-swarm-forks`, `vertex-swarm-api`. Their dependency cone resolved for `wasm32-unknown-unknown` with `--no-default-features` is free of chain code and of `reqwest`, and `just check-cone` enforces that. A plain `cargo build` for wasm does not yet succeed: `nectar-primitives` pulls `wasm-bindgen-rayon`, which needs a threaded-wasm toolchain (`atomics` + `bulk-memory` + `build-std`), and `vertex-swarm-spec`/`vertex-swarm-api` pull `vertex-tasks`, whose `tokio::select!` usage needs the wasm tokio feature set trimmed per the tokio audit below. Both are tracked work, not a regression.
- Nectar primitives (`nectar-primitives`, `nectar-mantaray`, `nectar-postage`) are the upstream wasm-friendly layer. The proof-of-concept `crates/wasm-demo` lives in nectar.
- Legacy wasm bins in this repo (`bin/swarm-wasm-lib`, `bin/wasm-playground`) reference path deps to `crates/bmt` and `crates/postage` that no longer exist (they moved to nectar). They are stale and should not be treated as a working baseline; remove them or rewrite them against the current crate graph before adding new wasm code.
- A real client-in-wasm shipping target does not exist yet. The work plan is in this document.

### Crate boundary: who must compile for wasm

The wasm cone (must remain wasm-compatible):

- `vertex-swarm-primitives`, `vertex-swarm-spec`, `vertex-swarm-forks`, `vertex-swarm-identity`.
- `vertex-swarm-api` and any trait surface a client consumes.
- `vertex-swarm-bandwidth-core`, `vertex-swarm-bandwidth-pricing`, `vertex-swarm-bandwidth-pseudosettle` (accounting logic; no IO).
- `vertex-swarm-builder`'s client variant.
- All `nectar-*` deps.

Native-only (must NOT pull into the wasm cone):

- `vertex-storage-redb` and anything pulling in `mmap`-style IO. Use an alternative `Database` backend in wasm (in-memory or IndexedDB-backed).
- `crates/swarm/topology`'s NAT discovery and netdev paths; the wasm client uses libp2p browser transports (websockets, WebTransport) and skips local-network classification.
- `crates/net/local`, `crates/net/dialer` (assumes native dial semantics), `crates/net/dnsaddr` (will need a wasm DNS-over-HTTPS shim if used at all).
- Storer node, bootnode, redistribution agent, RPC server.
- `bin/vertex` itself; the binary is native-only.

The grey zone (must be cfg-gated):

- `vertex-tasks`: the wasm client uses `wasm-bindgen-futures::spawn_local`, not multi-thread tokio. Gate the executor type.
- `vertex-observability`: tracing-subscriber works in wasm via `tracing-wasm`; Prometheus HTTP server does not. Gate the exporter and HTTP server modules.
- Anything pulling `tokio` features: under wasm we need `rt` only, no `rt-multi-thread`, no `net`, no `signal`, no `fs`.
- `getrandom`: the workspace must select the `js` feature for wasm targets (`getrandom = { features = ["js"] }` under a `cfg(target_arch = "wasm32")` table or via the `wasm` feature pattern).

### cfg-gating rules

Pick one of two patterns and stick to it per crate. Mixing both is the source of all wasm build breakages.

**Pattern A: `target_arch = "wasm32"`** for transport-level code where the choice is platform-determined and the consumer should not have to opt in. Use this for runtime selection (`tokio::spawn` vs `wasm_bindgen_futures::spawn_local`), getrandom backend, and similar. Example:

```rust
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn<F: Future + Send + 'static>(f: F) { tokio::spawn(f); }

#[cfg(target_arch = "wasm32")]
pub fn spawn<F: Future + 'static>(f: F) { wasm_bindgen_futures::spawn_local(f); }
```

**Pattern B: cargo features** (`std`, `wasm`, `native`) for capability-level code where a single crate offers two implementations and the consumer crate picks one. Use this for storage backends (`vertex-storage-redb` is feature-gated out in wasm builds; an in-memory `Database` impl is feature-gated in) and observability sinks. Default features should produce the native build; the wasm build sets `default-features = false` and selects the wasm feature.

Rules that apply to both:

- No `#[cfg(target_arch = "wasm32")]` inside a function body. Hoist to a function-level cfg and provide two implementations. This keeps the code path obvious to a reader and gives clippy a clean signal.
- `#[cfg(not(target_arch = "wasm32"))]` blocks must have a matching wasm sibling. If the wasm path is "not supported", make the function return `Result` with a documented error variant, not a panic.
- New crates added to the wasm cone must include `wasm32-unknown-unknown` in their `[lints]` or `[lints.target.'cfg(target_arch = "wasm32")']` table so clippy runs on both targets in CI.
- Tests use `#[cfg(target_arch = "wasm32")]` plus `wasm-bindgen-test` for wasm; standard `#[tokio::test]` for native. Do not write a single `#[test]` that depends on multi-thread.

### Runtime and transport

- Wasm client runtime: `wasm-bindgen-futures` for spawn, single-thread tokio (`features = ["rt", "sync", "macros", "time"]`) for utilities. No `rt-multi-thread`, no `net`, no `signal`.
- Transport: `libp2p-websocket-websys` and (when ready) `libp2p-webtransport-websys`. Plain TCP transport is unavailable in the browser. `crates/swarm/node`'s client variant must select the transport via cfg.
- Identify, ping, handshake, headers, hive, pricing, pseudosettle, pushsync, retrieval are wire-protocol crates and should compile for wasm with no source change. Verify by adding wasm32 to their CI matrix once the transport is wired.
- The handshake `0x99` multiaddrs encoding is byte-identical on both targets; do not branch on it.

### Storage in wasm

- The native default is `vertex-storage-redb`. In wasm: an `IndexedDb` backend behind the `Database` trait, or `InMemoryBackend` for a session-only client.
- The peer-manager persistence path and the local store both speak the `Database` trait, not the redb type. Keep that boundary clean and the wasm port is a backend swap, not a rewrite.

### Tokio feature hygiene

Audit `Cargo.toml` entries for `tokio` regularly. The default-features-on form (`tokio = "1"`) is a wasm break waiting to happen because it pulls `net`, `fs`, `signal`. Library crates should request the exact features they use (`features = ["sync", "macros", "rt"]`) and let the binary turn on `rt-multi-thread`.

### Plan to a working client-in-wasm

1. Remove or rewrite `bin/swarm-wasm-lib` and `bin/wasm-playground` so they reflect the current crate graph (or delete them with a follow-up to re-add when ready).
2. Add a `wasm32-unknown-unknown` build step to CI for the wasm-cone crates listed above. Start with `cargo build --target wasm32-unknown-unknown --no-default-features -p vertex-swarm-primitives -p vertex-swarm-spec -p vertex-swarm-forks -p vertex-swarm-api`.
3. Audit tokio features in every wasm-cone crate; trim to the minimum.
4. Add an `IndexedDb` `Database` backend (likely under `crates/storage/indexeddb`) gated on `cfg(target_arch = "wasm32")`.
5. Add `libp2p-websocket-websys` to `crates/swarm/node`'s client variant under wasm cfg.
6. Build `bin/vertex-wasm-client` (new) that composes the client builder, the wasm transport, the IndexedDB backend, and `wasm-bindgen-futures` as the executor.

Each step is its own PR. Do not bundle.
