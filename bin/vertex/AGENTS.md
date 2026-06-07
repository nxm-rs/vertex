# AGENTS: bin/vertex/

The shipped binary. It is intentionally thin: it picks a global allocator, builds a multi-thread tokio runtime, and delegates everything else to `vertex-node-commands::run_cli` and `vertex-swarm-builder`.

Root-level rules in `/AGENTS.md` apply here too. The notes below are the area-specific overlay.

## Dos

- Keep `main.rs` small. New CLI behaviour goes into `vertex-node-commands` or the relevant protocol args crate, not here.
- Gate optional allocator and profiling integrations behind cargo features (`jemalloc`, `heap-profiling`). The defaults are the safe path.
- When you add a feature flag, add it to the workspace CI matrix so the default and full-features builds are both exercised.
- Use `eyre::Result` only at the binary edge. Internal calls return their domain error.

## Donts

- Do not embed Swarm protocol logic in this crate. The protocol lives in `vertex-swarm-*`.
- Do not add startup logging here. Tracing comes from `VertexTracer` inside `run_cli` so spans are configured before any subsystem logs.
- Do not couple the binary to a specific node type. Bootnode, client, and storer flows all go through the builder.
- Do not add a second `#[global_allocator]` without removing the existing cfg. Two allocator candidates is a link error nobody wants to debug.

## Building and running

- `cargo build --release -p vertex` builds the binary.
- `cargo build --release -p vertex --features jemalloc` enables jemalloc.
- `cargo build --release -p vertex --features heap-profiling` turns on jemalloc heap profiling sampling via `MALLOC_CONF`.
- `just run -- <args>` runs the release binary against your local config.
