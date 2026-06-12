# swarm-demo

A browser WebAssembly app that runs a real Vertex Swarm client node. It mints an ephemeral identity, resolves the live mainnet bootnodes over DNS-over-HTTPS (with an embedded snapshot fallback), dials them over secure WebSockets, and renders the Kademlia topology building up: connected peer count, neighborhood depth, per-bin fill, the topology phase, and a scrolling log of topology events.

This crate is wasm-only (`crate-type = ["cdylib"]`) and is intentionally **not** a member of the workspace `[workspace] members`. It depends on the wasm-only client launch path (`vertex_swarm_node::launch_client`) and never builds for native, so adding it to the default workspace would break native `cargo build`. Build it with the wasm toolchain and Trunk as below.

## How it is wired

The native node builder (`vertex-swarm-builder`) pulls the redb database, the chain provider, the SWAP settlement service, and the gRPC server, none of which build for `wasm32`. So the demo does not use it. Instead it calls a narrow, wasm-buildable launch entrypoint added to `vertex-swarm-node`:

- `vertex_swarm_node::launch_client(identity, bootnodes)` composes a `ClientNode` (connection-limits + identify + topology + client protocols) over the browser WebSocket transport, spawns the node run loop and the peer-manager tick on the wasm executor, and returns the `TopologyHandle`.
- The node run loop owns a `!Send` libp2p swarm (the websocket transport futures are `!Send`), so it is spawned through `TaskExecutor::spawn_local_with_graceful_shutdown_signal`, a wasm-only sibling of the Send-bounded spawner that routes through `wasm_bindgen_futures::spawn_local`.

The wasm-bindgen surface in `src/lib.rs` is small: a `#[wasm_bindgen(start)]` `main` that calls the exported async `start`, plus a `SwarmDemo` handle exposing `readiness()` (a JS snapshot object) and `drainEvents()` (the buffered topology events). The UI in `src/ui.rs` renders into the document via `web-sys`, updated on a one-second poll loop.

## Build

The build needs a nightly toolchain (the `+atomics` target feature pulled in transitively by `nectar-primitives` is unstable), the `wasm32-unknown-unknown` target with `rust-src`, Trunk, and `wasm-bindgen-cli`. The required rustflags (`+atomics,+bulk-memory,+mutable-globals` and `getrandom_backend="wasm_js"`) live in the workspace `.cargo/config.toml` under `[target.wasm32-unknown-unknown]` and Trunk passes them through.

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

## A note on the live connection and threaded wasm

A live mainnet connection needs a real browser and network reachability to the bootnodes. The CI-able proof for this crate is the clean `trunk build` plus the wasm client-construction code compiling for `wasm32`. The live connect is verified by loading the deployed page in a browser, where peers appear over the first tens of seconds and the bins fill in.

What a headless browser smoke confirmed against this build: the wasm module boots, the UI panel mounts, an ephemeral overlay address is minted, and the app reaches the bootnode-resolution step. The page is cross-origin isolated and `SharedArrayBuffer` is available.

There is one open item for the fully-live in-browser run. `nectar-primitives` compiles its parallel BMT hashing on a `wasm-bindgen-rayon` Web Worker thread pool, so the module is built with `+atomics` and uses `Atomics.wait`/`waitAsync` on shared memory. Driving that to completion needs the module linked with shared, importable memory (`--shared-memory`/`--import-memory`/`--max-memory`/`--export=__heap_base` plus the TLS exports), `-Z build-std`, and a `wasm-bindgen-rayon` `initThreadPool(...)` call wired into the bootstrap before the client runs. That linker recipe builds cleanly and `wasm-bindgen` emits the worker glue, but the worker-module URL the rayon bootstrap requests did not resolve under the local static server used for the smoke, so the worker pool did not come up there. Resolving the worker-URL wiring is folded into the phase-4 GitHub Pages deploy, where the served paths are stable; this crate ships the non-threaded boot that renders the UI, and the build pipeline plus the COOP/COEP and `coi-serviceworker.js` isolation are in place for it.
