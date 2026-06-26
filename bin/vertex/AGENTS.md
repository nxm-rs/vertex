# AGENTS: bin/vertex/

The shipped binary. It is intentionally thin: it picks a global allocator, builds a multi-thread tokio runtime, and delegates everything else to `vertex-node-commands::run_cli` and `vertex-swarm-builder`.

Root-level rules in `/AGENTS.md` apply here too. The notes below are the area-specific overlay.

## Build cones

The default `vertex` build is a bare client: it compiles neither the storer code cone (reserve, puller, batch store, redistribution, the `StorerNode` composite) nor the chain or swap stacks. Selecting `--features storer` compiles the full storer node and pulls the chain and swap stacks with it; a default binary asked to run a storer at runtime returns an error rather than panicking, telling the operator to rebuild with `--features storer`. The `chain` and `swap` features add their stacks on their own for a swap-capable client without the storer cone.

The cone guard (`just check-cone`, mirrored in the `features` CI job) enforces that the default tree never resolves the storer crates. The CI matrix must exercise at least the default client build, the `--features storer` build, and the existing `--all-features` build. These features follow the Feature and cfg contract in `/AGENTS.md`: they name capabilities, not node types, and `default = []` is the load-bearing bare client.

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

## Versioning

`--version` prints the package version plus the short commit sha, for example `0.1.0 (abc1234)`. `build.rs` stamps the sha via `vergen-gitcl` into `VERGEN_GIT_SHA`; outside a git checkout (the Docker build context excludes `.git`) it degrades to `unknown`. The version string lives in `cli.rs`, not `main.rs`.

## Releasing

Maintainers cut releases locally with `cargo release <level> --execute`, which bumps the workspace version, regenerates `CHANGELOG.md`, and creates the signed `vX.Y.Z` tag. Pushing the tag triggers the cargo-dist binary matrix (`release.yml`) and the multi-arch Docker build (`docker.yml`). The matrix ships the default bare client for five targets: `x86_64`/`aarch64` Linux (gnu), `x86_64`/`aarch64` macOS, and `x86_64` Windows (msvc). Full flow in `RELEASING.md`. The storer build is `--features storer`; the release artefacts are the default bare client only.
