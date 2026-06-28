# AGENTS: bin/vertex/

The shipped binary. Intentionally thin: it picks a global allocator, builds a multi-thread tokio runtime, and delegates everything else to `vertex-node-commands::run_cli` (via `cli.rs`) and `vertex-swarm-builder`.

Global rules: see root `/AGENTS.md`. The notes below are the area-specific overlay.

## Build cones

The default `vertex` build is a bare client: no storer code cone (reserve, puller, batch store, redistribution, the `StorerNode` composite) and no chain or swap stacks. `--features storer` compiles the full storer node and pulls the chain and swap stacks with it; a default binary asked to run a storer at runtime returns an error telling the operator to rebuild with `--features storer`, never panics. `chain` and `swap` add their stacks on their own for a swap-capable client without the storer cone.

The cone guard (`just check-cone`, mirrored in the `features` CI job) enforces that the default tree never resolves the storer crates. The CI matrix exercises at least the default client, the `--features storer`, and the `--all-features` builds. Per the Feature and cfg contract in `/AGENTS.md`, these features name capabilities, not node types, and `default = []` is the load-bearing bare client.

## Dos

- Keep `main.rs` small. New CLI behaviour goes into `vertex-node-commands` or the relevant protocol args crate, not here.
- jemalloc is the default global allocator wherever supported (Linux and macOS), pulled by a target-gated dependency, not a cargo feature. Windows (no msvc support) and wasm fall back to the system allocator. Gate further profiling integrations (`heap-profiling`) behind cargo features.
- When you add a feature flag, add it to the CI matrix so default and full-features builds are both exercised.
- Use `eyre::Result` only at the binary edge. Internal calls return their domain error.

## Donts

- No Swarm protocol logic here. The protocol lives in `vertex-swarm-*`.
- No startup logging here. Tracing comes from `VertexTracer` inside `run_cli` so spans are configured before any subsystem logs.
- Do not couple the binary to a node type. Bootnode, client, and storer flows all go through the builder.
- Do not add a second `#[global_allocator]` without removing the target-gated jemalloc one. Two candidates is a link error.

## Building and running

- `cargo build --release -p vertex` builds the binary; jemalloc is the default allocator on Linux and macOS.
- `cargo build --release -p vertex --features heap-profiling` turns on jemalloc heap-profile sampling via `MALLOC_CONF` (unix-only).
- `just run -- <args>` runs the release binary against your local config.

## Versioning

`--version` prints the package version plus the short commit sha (`0.1.0 (abc1234)`). The version lives in one place, `vertex-node-core::version`: its `build.rs` stamps the sha via `vergen-gitcl` into `VERGEN_GIT_SHA`, and `cli.rs` reads `version::LONG_VERSION` for the clap `--version` string. Outside a git checkout (the Docker build context excludes `.git`) it degrades to `unknown`. The binary carries no `build.rs` of its own. The same source drives the libp2p identify agent string (`version::AGENT_VERSION`, `vertex/<version>-<sha>`).

## Releasing

Maintainers cut releases locally with `cargo release <level> --execute`, which bumps the workspace version, regenerates `CHANGELOG.md`, and creates the signed `vX.Y.Z` tag. Pushing the tag triggers the cargo-dist binary matrix (`release.yml`) and the multi-arch Docker build (`docker.yml`). The matrix ships the default bare client for five targets: `x86_64`/`aarch64` Linux (gnu), `x86_64`/`aarch64` macOS, and `x86_64` Windows (msvc). The release artefacts are the default bare client only; the storer build is `--features storer`. Full flow in `RELEASING.md`.
