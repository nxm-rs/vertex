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

The build needs a nightly toolchain (the `+atomics` target feature pulled in transitively by `nectar-primitives` is unstable, and the threaded build rebuilds std from source), the `wasm32-unknown-unknown` target with `rust-src`, Trunk, and `wasm-bindgen-cli`. The threaded-wasm linker recipe (shared memory, the TLS and heap exports, `build-std`, and `getrandom_backend="wasm_js"`) lives in this crate's own `.cargo/config.toml` under `[target.wasm32-unknown-unknown]`, which fully replaces the workspace config because this crate is its own workspace root. Trunk passes it through, so a plain `trunk build --release` needs no extra flags.

On the project's Nix host, the whole toolchain is available through a one-off shell:

```sh
nix-shell -E '
  let
    rustOverlay = import (builtins.fetchTarball "https://github.com/oxalica/rust-overlay/archive/master.tar.gz");
    pkgs = import <nixpkgs> { overlays = [ rustOverlay ]; };
    toolchain = pkgs.rust-bin.nightly.latest.default.override {
      extensions = [ "rust-src" ];
      targets = [ "wasm32-unknown-unknown" ];
    };
  in pkgs.mkShell {
    buildInputs = [ toolchain pkgs.trunk pkgs.wasm-bindgen-cli pkgs.binaryen pkgs.protobuf pkgs.pkg-config pkgs.openssl ];
  }
'
```

Then, from this directory:

```sh
RUSTUP_TOOLCHAIN=nightly trunk build --release
```

This produces `dist/` with the wasm module, the wasm-bindgen JS glue, `index.html`, `styles.css`, and the copied `coi-serviceworker.js`.

Prebuilt `wasm32-unknown-unknown` std is used (no `-Z build-std`). If your toolchain lacks prebuilt wasm std, add `rust-std` for the target; do not switch on `-Z build-std`, which rebuilds std and drags in native-only crates that do not compile for wasm.

## Serve locally

```sh
RUSTUP_TOOLCHAIN=nightly trunk serve --release
```

Then open http://127.0.0.1:8080. `trunk serve` sends the `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp` headers (see `Trunk.toml`), so the page is cross-origin isolated and SharedArrayBuffer is available.

## Cross-origin isolation on GitHub Pages

GitHub Pages cannot set response headers, so the vendored `coi-serviceworker.js` (referenced from `index.html` and copied into `dist/`) registers a service worker that re-serves every response with the COOP/COEP headers, making the page cross-origin isolated client-side. Locally it is a harmless no-op since Trunk already sets the headers. Wiring the Pages deploy is a later phase; this crate just ships the shim and the reference.

## Threaded wasm and the rayon thread pool

The wasm cone is compiled with `+atomics` because `nectar-primitives` pulls `wasm-bindgen-rayon`. That has one runtime consequence the connect/topology demo actually depends on: `wasm-bindgen-futures` schedules its task queue with `Atomics.waitAsync`, which only works on a `SharedArrayBuffer`. So the module is linked with shared, importable memory (`--shared-memory --import-memory --max-memory` plus the TLS and heap exports) and `build-std`, all from `.cargo/config.toml`.

The demo never calls `initThreadPool` and never spawns a rayon worker: its connect and topology path does no parallel hashing (BMT hashing on wasm is sequential, and the demo uploads nothing). Shared memory alone is what makes the executor run; with it, no worker pool is needed and the rayon worker-URL question never arises. `Trunk.toml` keeps `filehash = false` so the emitted module names stay stable, which is what would let the rayon worker bootstrap resolve its relative module URL if a future feature did spin up the pool.

## Verifying the live connection

A headless browser run against this build (served with COOP/COEP) boots the module, mounts the UI, mints an overlay, resolves the mainnet bootnodes over DoH, and dials them over secure WebSockets: the wss/TLS connection to the live `libp2p.direct` AutoTLS endpoint opens and the libp2p upgrade (noise/multistream) is attempted. Completing the upgrade to a connected peer needs network reachability to the bee nodes behind that endpoint, which a sandboxed CI network may not have. The CI-checkable proof is the clean `trunk build` plus the wasm client cone compiling; the live connect is confirmed by loading the deployed page in a real browser, where peers appear and the bins fill in.
