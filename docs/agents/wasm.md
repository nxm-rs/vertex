## Wasm target guidance

Vertex aims to run a **client** node type in the browser (`wasm32-unknown-unknown`). This is a planning constraint that shapes every crate, not a future migration. Bootnode and storer node types are explicitly out of scope for wasm; they will only ever run natively.

### Why

A wasm client unlocks light-client embeddings in dapps, indexer UIs, and the Nexum gateway without a separate codebase. The Swarm domain logic and the higher-level protocol code can be the same in both worlds; only transport, runtime, storage, and observability differ.

### Status

- Already no_std capable with a `default = ["std"]` feature: `vertex-swarm-primitives`, `vertex-swarm-spec`, `vertex-swarm-forks`, `vertex-swarm-api`.
- The peer stack (`vertex-swarm-peer`, `vertex-swarm-peer-score`, `vertex-swarm-peer-manager` plus their `vertex-net-peer-*` and `vertex-net-local` deps) builds for `wasm32-unknown-unknown` and CI enforces it (the `wasm` job in `.github/workflows/unit.yml`). The build currently requires a nightly toolchain: `nectar-primitives` pulls `wasm-bindgen-rayon` on wasm32, which needs the unstable `atomics` target feature that stable rustc does not expose. The required rustflags (`+atomics,+bulk-memory,+mutable-globals` and the `getrandom_backend="wasm_js"` cfg) live in `.cargo/config.toml` under `[target.wasm32-unknown-unknown]`, mirroring nectar's own config. Run it with `cargo +nightly build --target wasm32-unknown-unknown -p vertex-util-runtime -p vertex-swarm-peer-score -p vertex-swarm-peer-manager -p vertex-swarm-identity`. Making this buildable on stable is upstream work in nectar (gate the `wasm-bindgen-rayon` pull behind a feature).
- The full client cone (`vertex-swarm-node`) builds for `wasm32-unknown-unknown` and CI enforces it (the `wasm` job builds `-p vertex-swarm-node`). This pulls the topology composite behaviour and every `/swarm/...` wire protocol. The three native-only blockers in the topology cone were resolved: `vertex-observability` was split so the `axum` metrics server is a native-default feature, `vertex-net-dnsaddr` bootnode resolution is cfg-gated to native with a `vertex-net-dnsaddr-doh` (DNS-over-HTTPS) sibling on wasm, and the `if-watch` netdev interface watcher is cfg-gated to native with a no-op wasm sibling. Topology and the wire crates pull a trimmed wasm libp2p (no tcp/dns/mdns/upnp). A headless mainnet connection cannot run in CI (it needs a browser and the network); the compile is the CI-enforced proof, and the live connect is exercised by the browser demo. Inside `vertex-swarm-node`, NAT traversal (AutoNAT v2, UPnP) and LAN discovery (mDNS) live in a platform module (`node/nat.rs` with the `node/nat_wasm.rs` sibling, Pattern C below): the node composites carry a single `nat` field on both targets, and the wasm sibling is a no-op behaviour with an uninhabited event enum.
- Time and randomness for the wasm cone live in one cfg-gated home, `vertex-util-runtime` (`crates/util/runtime`). Its `time` module re-exports the `web-time` clock types (a `std::time` re-export on native, browser clock on wasm32) and adds deduplicated Unix-timestamp helpers (`now_unix_secs`, `now_unix_millis`, `now_unix_nanos`, `now`); its `rand` module is a getrandom-backed facade (`fill_bytes`, `crypto_rng`, `non_crypto_rng`, `try_*`) standardized on rand 0.9 / getrandom 0.3.4. There are no thread-local RNGs. Reach for `vertex_util_runtime::time` and `vertex_util_runtime::rand` rather than importing `web-time`, `getrandom`, or `rand` directly. The single intentional `web-time` dependency is `vertex-util-runtime` itself; everything else routes through the facade.
- Identity in the wasm cone is ephemeral by design. The keystore (`vertex-swarm-identity` on-disk keystore) is native-only; a wasm client constructs an in-memory `Identity::random()` per session rather than persisting a key. There is no wasm keystore and none is planned.
- Nectar primitives (`nectar-primitives`, `nectar-mantaray`, `nectar-postage`) are the upstream wasm-friendly layer. The proof-of-concept `crates/wasm-demo` lives in nectar.
- Legacy wasm bins in this repo (`bin/swarm-wasm-lib`, `bin/wasm-playground`) reference path deps to `crates/bmt` and `crates/postage` that no longer exist (they moved to nectar). They are stale and should not be treated as a working baseline; remove them or rewrite them against the current crate graph before adding new wasm code.
- A real client-in-wasm shipping target does not exist yet. The work plan is in this document.

### Chain code in wasm

Chain code is wasm-compatible and is welcome in the wasm cone. `alloy-provider`, `alloy-contract`, and the alloy signer/transport stack build for `wasm32-unknown-unknown` when their features are selected to avoid wasm-incompatible or native-only deps (notably `native-tls`/`openssl`, a default `reqwest` TLS backend, and threaded `rayon`). Pick the wasm-friendly transport and TLS features for alloy, the same way every other crate trims its features for wasm, and the chain client compiles. Do not architect a "no chain in wasm" boundary: an earlier attempt to forbid chain code from the wasm and light-node cones created a structural bottleneck that pushed later PRs into reimplementing pieces of alloy by hand, which is wasted effort. The rule is "the wasm-targeted crates compile for wasm with our chosen features", not "chain code is absent".

Strict primitive crates (pure data, no network and no database) should aim for `no_std` where it is sensible, since that keeps them trivially wasm-buildable and reusable by non-node consumers. Do not over-engineer this: if a primitive needs `alloc` or a small std-only dependency and the cost of going `no_std` is high, leave it `std` and move on. The goal is wasm-buildability, not `no_std` purity for its own sake.

### Crate boundary: who must compile for wasm

The wasm cone (must remain wasm-compatible):

- `vertex-swarm-primitives`, `vertex-swarm-spec`, `vertex-swarm-forks`, `vertex-swarm-identity`.
- `vertex-swarm-api` and any trait surface a client consumes.
- `vertex-swarm-bandwidth-core`, `vertex-swarm-bandwidth-pricing`, `vertex-swarm-bandwidth-pseudosettle` (accounting logic; no IO).
- The peer stack: `vertex-swarm-peer`, `vertex-swarm-peer-score`, `vertex-swarm-peer-manager`, and their net-layer deps `vertex-net-peer-backoff`, `vertex-net-peer-score`, `vertex-net-peer-store`, `vertex-net-peer-registry`. These are tick-driven and timer-free; the periodic driver runs on the composition side.
- `crates/net/local`: pure multiaddr scope classification and capability tracking, no socket IO; it sits in the peer stack's dependency cone.
- `crates/swarm/topology`: the composite `TopologyBehaviour` is part of the browser `ClientNodeBehaviour`, so the crate builds for wasm. Its netdev interface watcher (`if-watch`) and system-DNS bootnode resolution (`vertex-net-dnsaddr`) are native-only target deps with wasm siblings (a no-op watcher and DoH resolution); the wasm build pulls a trimmed libp2p (no tcp/dns/mdns/upnp) the same way `vertex-swarm-node` does. The `wasm` CI job builds `-p vertex-swarm-node`, which pulls the whole topology cone.
- `crates/net/dialer`: the `DialTracker` speaks libp2p `Multiaddr`/`PeerId`/`ConnectionId` vocabulary only and builds on both targets with the trimmed wasm libp2p. The wasm swarm still dials through it; address filtering and in-flight tracking are platform-neutral.
- `vertex-swarm-node`'s client variant. `vertex-swarm-node` is the milestone wasm crate for "build a client node": it composes topology and every `/swarm/...` wire protocol into the browser `ClientNodeBehaviour`. `vertex-swarm-builder` is native-only because it also pulls `vertex-storage-redb`, the RPC server, redistribution, and the storer/bootnode builders.
- All `nectar-*` deps.

Native-only (must NOT pull into the wasm cone):

- `vertex-storage-redb` and anything pulling in `mmap`-style IO. Use an alternative `Database` backend in wasm (in-memory or IndexedDB-backed).
- `crates/net/dnsaddr` (native system resolver via `hickory-resolver`). The browser uses `vertex-net-dnsaddr-doh` (DNS-over-HTTPS) instead; topology selects between them with a `cfg(target_arch)` target table.
- Storer node, bootnode, redistribution agent, RPC server.
- `bin/vertex` itself; the binary is native-only.

The grey zone (must be cfg-gated):

- `vertex-tasks`: the wasm client uses `wasm-bindgen-futures::spawn_local`, not multi-thread tokio. Done: the spawn choke point (`TaskExecutor::spawn_on_rt`) and the `TaskHandle` return type are cfg-gated (Pattern A). Native `TaskHandle` is `tokio::task::JoinHandle<()>` so the native API is byte-identical; the wasm sibling is an abortable no-op wrapper over a `futures_util::future::AbortHandle`, and both Default and Blocking task kinds map to the same `spawn_local`. The `wasm` job in CI builds `-p vertex-tasks`.
- `vertex-observability`: split with Pattern B into orthogonal slices (`subscriber`, `otlp`, `prometheus`, `http-server`) plus a `host` umbrella that unions them. The native infrastructure (tracing subscriber, OTLP exporters, the Prometheus recorder, the metrics HTTP server via `axum`, and profiling) lives behind those slices; `http-server` pulls `axum` -> `tokio[net]` -> `mio` and is native-only. The crate defaults to no features (the light surface), so wasm-cone crates and embedders get only the platform-neutral primitives for free: the plain config structs (`StdoutConfig`, `OtlpConfig`, `OtlpLogsConfig`, `MetricsServerConfig`, `LogFormat`, all dependency-free), the recording macros, RAII guards, label utilities (re-exported from `vertex-metrics`), and the histogram bucket presets plus `HistogramBucketConfig`. Native consumers opt into the minimal slice they use: `vertex-node-core` enables nothing, `vertex-node-commands` enables `otlp`, `vertex-node-builder` enables `http-server`, and `bin/vertex` enables `host`.
- Anything pulling `tokio` features: under wasm we need `rt` only, no `rt-multi-thread`, no `net`, no `signal`, no `fs`.
- `getrandom`: three major lines coexist because different transitive deps pin different versions. The 0.3 and 0.4 lines select their browser backend through the `getrandom_backend="wasm_js"` cfg in `.cargo/config.toml`; the 0.2 line (reached transitively through `k256`/`rand_core 0.6` and the libp2p/TLS stack) selects its backend through the `js` cargo feature instead. Two crates carry hand-written `cfg(target_arch = "wasm32")` getrandom feature tables for that 0.2 line: `vertex-swarm-primitives` (the alloy-primitives `getrandom` feature plus the 0.4 `wasm_js` backend, required by alloy nonce generation) and `vertex-swarm-bandwidth-chequebook` (the 0.2 `js` feature, required by the k256 secp256k1 backend). Both are load-bearing transitive build requirements; removing either breaks the wasm build with a getrandom no-backend error. Application randomness goes through `vertex_util_runtime::rand`, not a direct getrandom dependency.

### cfg-gating rules

Pick one of these patterns and stick to it per crate. Mixing them is the source of all wasm build breakages.

**Pattern A: `target_arch = "wasm32"`** for transport-level code where the choice is platform-determined and the consumer should not have to opt in. Use this for runtime selection (`tokio::spawn` vs `wasm_bindgen_futures::spawn_local`), getrandom backend, and similar. Example:

```rust
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn<F: Future + Send + 'static>(f: F) { tokio::spawn(f); }

#[cfg(target_arch = "wasm32")]
pub fn spawn<F: Future + 'static>(f: F) { wasm_bindgen_futures::spawn_local(f); }
```

**Pattern B: cargo features** (`std`, `wasm`, `native`) for capability-level code where a single crate offers two implementations and the consumer crate picks one. Use this for storage backends (`vertex-storage-redb` is feature-gated out in wasm builds; an in-memory `Database` impl is feature-gated in) and observability sinks. For capability backends the default features produce the native build and the wasm build sets `default-features = false`; `vertex-observability` inverts the polarity (it defaults to the light surface and native consumers opt into the `host` slice) because the light surface is the common case for the wasm cone and embedders.

**Pattern C: platform module.** When a single target cfg would touch three or more items in one file, or would gate a struct field, extract a sibling module selected with `#[cfg_attr(target_arch = "wasm32", path = "foo_wasm.rs")] mod foo;`. Both files export the same item names with identical signatures so every call site compiles unchanged on both targets. Models: `vertex-swarm-topology` (`tasks.rs`/`tasks_wasm.rs`) and `vertex-swarm-node` (`nat.rs`/`nat_wasm.rs`).

Rules that apply to all of them:

- Never cfg-gate struct fields or enum variants. Wrap the divergent part in a platform type (Pattern C) with a uniform constructor. For a `NetworkBehaviour` composite, the wasm sibling is a one-field wrapper around `libp2p::swarm::dummy::Behaviour` whose `to_swarm` event is an uninhabited enum; the shared composite keeps one field, one event variant, one `From` impl, one match arm. Do not derive `NetworkBehaviour` on a zero-field struct.
- Sibling signatures are byte-identical: same parameters, same (or alias-equivalent) return types. The wasm side accepts and ignores arguments rather than changing arity.
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

### Wall clocks and timers in the wasm cone

Two distinct hazards, both of which surface only at runtime (a clean wasm build still panics on the first clock read):

- Wall clock and monotonic time go through `web-time`, never `std::time`. On `wasm32-unknown-unknown` the std clock is the unsupported-platform stub and panics ("time not implemented on this platform"). Reach for `vertex_util_runtime::time::{Instant, SystemTime, now_unix_secs}`, which re-export `web-time`. The same rule holds upstream: `nectar_primitives::Timestamp::now` and the postage stamp timestamp also read the clock through `web-time` (nectar 0.2.0+).
- All timer primitives come from `vertex_tasks::time`: `sleep`, `interval`/`interval_after` (an `Interval` with `poll_tick` for behaviour poll loops and async `tick` for tasks), `timeout` (returns `Result<T, Elapsed>`), and `Instant`. Never `tokio::time` in wasm-cone code, including `tokio::time::Instant`. The tokio timer driver does not run on wasm32: `tokio::time::{sleep, interval, timeout, Instant::now}` reach the std clock and panic. On native the `vertex_tasks::time` primitives are tokio's; on wasm they run on the browser's `setTimeout` through `gloo-timers`.
- Two clocks rule: `vertex_tasks::time::Instant` is the timer clock (tokio's pause-aware clock on native, the browser clock on wasm); use it in async or polled code and anywhere paused-time tests advance time. `vertex_util_runtime::time::{Instant, SystemTime, now_unix_*}` remain the wall and plain monotonic clocks for timer-free, tick-driven crates.
- The Send divergence has one home: the browser timer and fetch futures are `!Send`, so hold timers as `vertex_tasks::time::BoxTimerFuture` and other platform-Send boxed futures as `vertex_tasks::MaybeSendBoxFuture<T>`. Do not define per-crate cfg-gated future aliases. Tasks that own such a future spawn through the executor's local (`!Send`) path on wasm.
- Native-only tests may use `tokio::time` directly (including `pause`/`advance`); wasm-bindgen tests must use `vertex_tasks::time`.
- `futures-timer` is allowed only in code that mirrors upstream libp2p handlers (`vertex-swarm-net-identify`), and only via the workspace dep so the wasm-bindgen browser-clock feature applies (libp2p also pulls it transitively for connection-handler delays; the workspace feature keeps those off the std clock).

### Browser dial capability

A browser client has no listen addresses, so the listen-address-derived `IpCapability` heuristic leaves it at `None` and the dialer filters every candidate as unreachable. The browser can in fact open outbound connections to either address family, so `vertex_net_local::LocalCapabilities::capability` returns `IpCapability::Dual` on wasm32. The bee AutoTLS wss leaf form `/ip4/<ip>/tcp/<port>/tls/sni/<host>/ws` is not dialable by `libp2p-websocket-websys` (it rejects the `/sni` component); `vertex-net-dnsaddr-doh` rewrites accepted leaves to `/dns4/<host>/tcp/<port>/tls/ws`, which the browser dials by hostname with the TLS SNI following from it.

### The browser demo: `bin/swarm-demo`

`bin/swarm-demo` is a wasm-only `cdylib` that runs a real client node in the browser: it mints an ephemeral identity, resolves the mainnet bootnodes over DoH, dials them over secure websockets, and renders the live Kademlia topology. It is its own workspace root (an empty `[workspace]` table) and is excluded from the native workspace, so `cargo build` never tries to compile it for native.

Build and serve with Trunk (`bin/swarm-demo/README.md` has the full toolchain shell):

- `trunk build --release` (nightly toolchain) produces `dist/` with the wasm module, the wasm-bindgen glue, the HTML shell, the stylesheet, and the vendored `coi-serviceworker.js`.
- The threaded-wasm linker recipe lives in `bin/swarm-demo/.cargo/config.toml`: `build-std` plus shared, importable memory (`--shared-memory --import-memory --max-memory`) and the TLS/heap exports the wasm-bindgen threading transform needs. Shared memory is required not for rayon (the connect path never parallelizes and never calls `initThreadPool`) but because `wasm-bindgen-futures` schedules its task queue with `Atomics.waitAsync`, which only works on a `SharedArrayBuffer`. `Trunk.toml` sets `filehash = false` (so the rayon worker bootstrap's relative module URL still resolves) and `wasm_opt = false` (binaryen needs `--enable-threads` for the shared-memory module).
- SharedArrayBuffer needs cross-origin isolation (COOP/COEP). `trunk serve` sets those headers directly; on GitHub Pages, which cannot set response headers, the vendored `coi-serviceworker.js` injects them client-side.

Deploy: `.github/workflows/pages.yml` builds the demo with `--public-url /vertex/` (the Pages subpath for `nxm-rs/vertex`) and publishes `dist/` through `actions/deploy-pages`. The demo is the whole Pages site.

### Plan to a working client-in-wasm

1. Done: `bin/swarm-wasm-lib` and `bin/wasm-playground` removed; `bin/swarm-demo` is the live browser client.
2. Add a `wasm32-unknown-unknown` build step to CI for the wasm-cone crates listed above. Done for the peer stack (the `wasm` job builds `vertex-swarm-peer-score` and `vertex-swarm-peer-manager`, which pulls the whole peer cone) and for `vertex-tasks`; extend the `-p` list as more cone crates become buildable.
3. Audit tokio features in every wasm-cone crate; trim to the minimum.
4. Add an `IndexedDb` `Database` backend (likely under `crates/storage/indexeddb`) gated on `cfg(target_arch = "wasm32")`.
5. Done: `libp2p-websocket-websys` is wired into `crates/swarm/node`'s client variant under wasm cfg (`build_swarm`).
6. Build a richer browser client on top of `vertex_swarm_node::ClientLauncher` (the fluent launcher shared by native embedders and the browser; `bin/swarm-demo` launches through it) that adds the IndexedDB backend and upload/download flows.

Each step is its own PR. Do not bundle.
