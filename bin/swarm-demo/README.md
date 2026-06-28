# swarm-demo

A browser WebAssembly app that runs a real Vertex Swarm client node. It mints an ephemeral identity, resolves the live mainnet bootnodes over DNS-over-HTTPS (with an embedded snapshot fallback), dials them over secure WebSockets, and renders the Kademlia topology building up: connected peer count, neighborhood depth, per-bin fill, the topology phase, and a scrolling log of topology events.

Deployed at https://nxm-rs.github.io/vertex/ via `.github/workflows/pages.yml`.

This crate is wasm-only (`crate-type = ["cdylib"]`) and is intentionally **not** a member of the workspace `[workspace] members`. It targets the browser shape of the client launch path and never builds for native, so adding it to the default workspace would break native `cargo build`. Build it with the wasm toolchain and Trunk as below.

## How it is wired

The native node builder (`vertex-swarm-builder`) pulls the redb database and the gRPC server, neither of which builds for `wasm32`. So the demo does not use it. Instead it goes through the fluent launcher in `vertex-swarm-node`, shared by native embedders and the browser:

- `vertex_swarm_node::ClientLauncher::new(identity).with_bootnodes(bootnodes).launch()` composes a `ClientNode` (connection-limits + identify + topology + client protocols) over the browser WebSocket transport, spawns the node run loop and the peer-manager tick on the wasm executor, and returns a `LaunchedClient` whose `topology()` accessor hands the demo its `TopologyHandle`.
- The node run loop owns a `!Send` libp2p swarm (the websocket transport futures are `!Send`), so it is spawned through `TaskExecutor::spawn_local_with_graceful_shutdown_signal`, a wasm-only sibling of the Send-bounded spawner that routes through `wasm_bindgen_futures::spawn_local`.
- The client cache is the IndexedDB-backed `ChunkStore` (`vertex-storage-indexeddb` mirrored through `vertex-swarm-localstore`'s `indexeddb` feature), supplied through `with_store`, so cached chunks survive a page reload. A failed open falls back to the in-memory default.
- SWAP cheque settlement (`vertex-swarm-node`'s `swap`/`swap-chequebook` features) is wired through `with_swap` when a chequebook address is supplied in the page URL query string. Cheque exchange is chain-free; an RPC URL turns on on-chain cashout.

Settlement and the cache read their optional config from the page URL: `?chequebook=0x...` enables SWAP, and an additional `&rpc=https://...` turns on cashout. Without a chequebook the client settles by pseudosettle alone.

The wasm-bindgen surface in `src/lib.rs` is small: a `#[wasm_bindgen(start)]` `main` that calls the exported async `start`, plus a `SwarmDemo` handle exposing `readiness()` (a JS snapshot object) and `drainEvents()` (the buffered topology events). The UI in `src/ui.rs` renders into the document via `web-sys`, updated on a one-second poll loop.

## Build

The build needs the pinned stable toolchain, the `wasm32-unknown-unknown` target, Trunk, and `wasm-bindgen-cli`. The demo is single-threaded, so it uses prebuilt std and ordinary linear memory; the only wasm rustflag is `getrandom_backend="wasm_js"` in this crate's own `.cargo/config.toml` under `[target.wasm32-unknown-unknown]`, which fully replaces the workspace config because this crate is its own workspace root. A plain `trunk build --release` needs no extra flags.

On the project's Nix host, the extra tooling is available through a one-off shell:

```sh
nix-shell -p trunk wasm-bindgen-cli binaryen protobuf
```

Then, from this directory:

```sh
trunk build --release
```

This produces `dist/` with the wasm module, the wasm-bindgen JS glue, `index.html`, and `styles.css`.

## Serve locally

```sh
trunk serve --release
```

Then open http://127.0.0.1:8080. The demo is single-threaded and uses no `SharedArrayBuffer`, so it needs no cross-origin isolation (COOP/COEP) headers.

## Verifying the live connection

A headless browser run against this build boots the module, mounts the UI, mints an overlay, resolves the mainnet bootnodes over DoH, and dials them over secure WebSockets: the wss/TLS connection to the live `libp2p.direct` AutoTLS endpoint opens and the libp2p upgrade (noise/multistream) is attempted. Completing the upgrade to a connected peer needs network reachability to the bee nodes behind that endpoint, which a sandboxed CI network may not have. The CI-checkable proof is the clean `trunk build` plus the wasm client cone compiling; the live connect is confirmed by loading the deployed page in a real browser, where peers appear and the bins fill in.
