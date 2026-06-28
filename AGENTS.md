# AGENTS.md

Canonical contract for any agent working in this repository (Claude Code, Codex, Cursor, OpenHands, or a human collaborator). `CLAUDE.md` at the same level is a symlink to this file; subdirectories that ship their own per-area `AGENTS.md` follow the same pattern.

Vertex is a Rust implementation of the Ethereum Swarm node, designed for modularity, performance, and client diversity. The dominant peer on the live network is the Go reference node; v1 conformance with its wire bytes is required so Vertex can acquire real users, while the internal architecture is free to be idiomatic Rust.

## Process: start every task here

Walk this checklist before writing code. Skip it only for typo and clippy-lint changes.

1. **Classify the change.** Wire-visible? Public-trait? Internal refactor? New feature? Bug fix? Each path has different rules below.
2. **Read the relevant guidance.** This file top to bottom; the per-area `AGENTS.md` for every directory you touch; the matching deep guide under `docs/agents/` (table below) for any wire, Rust-architecture, or libp2p question; the `docs/` pages linked from the area files; for protocol semantics, the relevant chapter of `docs/swarm/reference/book-of-swarm.txt`.
3. **Refine the scope and spec before code.**
   - Wire-visible change: define the exact bytes and gate behind a `SwarmHardfork` if it diverges from the reference. Add or update the conformance vectors under the protocol crate's `tests/`.
   - Public-trait change: write or update the design note (crate root rustdoc or `docs/design/`) and run it past the affected crates.
   - New CLI flag or config knob: place it where `crates/node/AGENTS.md` says it belongs.
4. **Update AGENTS.md before implementing.** If guidance for an area is missing, stale, or wrong, fix it in the same PR. Do not implement against guidance you know is wrong.
5. **Implement, then verify.** `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test -p <crate>`. Push. Watch `gh pr checks <N>` until green.

## Top of mind

Rules that catch the most review comments. None of these bend.

- **`multiaddrs`, never `underlay`.** Applies to code, comments, docs, commits, PR bodies.
- **No em-dashes.** ASCII hyphens or split the sentence. Source, rustdoc, markdown, commits, PR bodies, chat output.
- **No inline references to the reference implementation in code or operator-facing docs.** Brief architectural notes belong only at the crate root rustdoc, not scattered through call sites. Agent-only files under `docs/agents/` are the exception.
- **No "Unit N" internal plan labels in shipped rustdoc.** Describe consumers and components by name.
- **Rustdoc is terse by default; calibrate low.** State the intent plus the one non-obvious invariant a reader needs: a wire or byte layout, a consensus-observable rule, a real safety or ordering reason. No module essays, no `///` that restates the signature, no `//` that narrates the next line. Comment only what the code cannot say, once. Full guidance in `docs/agents/rust-idiomatic.md`.
- **Pre-commit is required, not polish.** `cargo fmt --all` and `cargo clippy --all-targets --all-features -- -D warnings`. Zero tolerance for unformatted or warning-bearing pushes.
- **Scope verification to the change; CI runs the full matrix.** Test the crates you touched (`cargo test -p <crate>`), not the whole workspace. Never run benches as a correctness gate or outside performance work. A doc or comment only change needs clippy (for `missing_docs`) and doctests only if a `///` fence changed, never the test suite. For a comment-only restack or pure move, prove code-equivalence with a filtered `git diff` instead of recompiling. Full rules in `docs/agents/rust-idiomatic.md`.
- **`git push` and `gh pr checks <N>` are one unit.** Watch CI until green. `MERGEABLE` is not the success signal.
- **No attribution in commits; AI disclosure required in PR bodies.** Commit messages stay clean: no "Co-Authored-By" lines, no robot footer. PR bodies REQUIRE a factual `AI Assistance: <tool> used for <parts>` line per the org guide `github.com/nxm-rs/.github` `CONTRIBUTING.md`. Omitting it risks PR closure or a ban.
- **No wire change without a fork gate.** Use `SwarmHardfork` and `ForkDigest`. Never feature-flag wire bytes with cargo features.
- **Primitives and layer-2 constructs live in `nectar`, not `vertex`.** See Repo split before adding chunk, addressing, manifest, feed, BMT, postage, or other domain-primitive code here.
- **Reach for the workspace derive macros before hand-rolling impls.** `thiserror`, `strum`, `derive_more`, `auto_impl(&, Arc, Box)`. Rules in `docs/agents/rust-idiomatic.md`.
- **A client node will run in wasm.** Plan every new crate for `wasm32-unknown-unknown`: pick `target_arch` vs feature cfg per `docs/agents/wasm.md`, audit tokio features, keep the wasm cone clean.
- **Public APIs are FFI and gRPC only. No HTTP+JSON.** Vertex is library-first: FFI (Dart bindings and similar) for native and mobile, gRPC for desktop and server operator scripting, wasm-bindgen for browsers. No `openapi.yml`, no `serde_json` in public paths, no HTTP handler frameworks. Rules in `docs/agents/api-surface.md`.

## Repo split: vertex vs nectar

Vertex owns the **node**: libp2p protocols, peer management, topology, storage backend, observability, CLI, the binary. `nectar` (https://github.com/nxm-rs/nectar) owns the **primitives and layer-2 constructs**: anything another Swarm consumer (light client, indexer, web tool, contract verifier) would want without a libp2p stack. Both repos are under nxm-rs control, so moving code across the boundary is a same-org PR.

Belongs in `nectar`: chunk types (`CAC`, `SOC`), span encoding, BMT hash and proofs; address types (`SwarmAddress`, `OverlayAddress` derivation), proximity order, bin math; manifests (mantaray nodes, traversal, edge encoding); feeds (epoch grid, lookup, SOC-based mutability); postage (batch contract decode, stamp signing and verification, bucket math); erasure coding, redundancy, recovery; any pure-data validation needing neither network nor database.

Belongs in `vertex`: libp2p `NetworkBehaviour`s and wire protocols; peer manager, topology, scoring, backoff, dialer; storage abstractions (`vertex-storage`) and backends (`vertex-storage-redb`); storer reserve, chunk store, redistribution agent; node lifecycle, builder, CLI, observability, RPC.

How to apply this:

- Before adding a type or function to a `vertex-swarm-*` crate, ask: would a non-node consumer want this? If yes, draft it in `nectar` (PR under `nxm-rs/nectar`) and depend on it from vertex.
- If you find primitive-shaped code already in vertex that belongs upstream, open an issue here and a migration PR in nectar. The workspace pins all nectar deps to the same git rev (`Cargo.toml`), so the move is one rev bump here once nectar merges.
- `vertex-swarm-primitives` is the canonical re-export surface. New nectar exports flow into the workspace through it, so consumers see one path.
- If something is genuinely vertex-only (a `Validated*` wrapper depending on a vertex storage trait, say), it stays here and a comment at the top of the type says why.

## Feature and cfg contract

Vertex ships three artefacts: a bare client (the default), a storer (`--features storer`), and the FFI client library (the `vertex-ffi` cdylib). The cone guards (`just check-cone`, the `features` CI job) enforce this split.

- `default = []` IS the bare client and is load-bearing: no storer cone, no chain, no swap. Never write `default = ["..."]` on a shipped crate.
- Features are for CAPABILITIES (`chain`, `swap`, the `storer` composite, observability slices), never for node TYPES. A node type is the runtime `SwarmNodeType`, dispatched at launch, never a per-type feature.
- `#[cfg(feature = ...)]` lives only at composition roots: `bin/vertex` (cli), `vertex-swarm-builder` (launch), `crates/ffi` (lib), and `vertex-node-builder` (the protocol-agnostic launch shell, where the optional `metrics` slice gates the Prometheus recorder and axum server). Domain crates (`client-behaviour`, `client-protocol`, `api`, `topology`, the node protocol) take their capabilities through traits and optional providers and carry no feature cfg, with one sanctioned exception: the `swap` capability. Swap cheque variants and their dispatch are gated inside `client-behaviour` and `client-protocol` because a swap-off build must exclude the swap cone (`vertex-swarm-net-swap`, the swap settlement crates, and the chain provider's swap pulls) at compile time to stay lean, and that cone cannot be reached through a runtime provider. This is interop-safe: a swap-off client speaking only pricing and pseudosettle is a normal peer. Do not "tidy" these gates away by always-compiling the swap codec; that drags the swap and chain cone into the bare and wasm client, which the swap cone guard exists to catch.
- Platform boundaries are `target_arch` cfg, never a feature. Never combine a feature and a target in one dependency table entry.
- FFI is a crate (the cdylib artefact), not a feature. There is no `ffi` feature anywhere; the crate boundary scopes it.
- A workspace member must not unconditionally enable `chain`, `swap`, or `storer` on a shared crate. Cargo unifies features across the build graph, so one such edge pulls the cone into the default client. This is the unification footgun the cone guards catch.

## Build, test, lint

- Edition `2024`, MSRV `1.92`. Do not raise MSRV without bumping the workspace `Cargo.toml` in the same commit.
- `cargo build --release -p vertex` builds the binary into `target/release/vertex`.
- `cargo test` runs workspace unit tests. Per-crate: `cargo test -p <crate>`. Integration tests live under each crate's `tests/`.
- `cargo fmt --all` formats. `cargo clippy --all-targets --all-features -- -D warnings` lints. Both required pre-commit.
- The `justfile` at repo root collects common workflows. When in doubt, read it.
- Missing tooling on this NixOS host: use `nix-shell -p <pkg> --run "..."`. The project shell is in `flake.nix`.

## Where rules live

| Area | File |
|---|---|
| Swarm wire conformance, fork gating, terminology, Book of Swarm anchors | `docs/agents/swarm-protocol.md` |
| Rust idioms, error model, async patterns, anti-Go-isms, testing | `docs/agents/rust-idiomatic.md` |
| libp2p boundary, NetworkBehaviour rules, codecs, PeerId vs OverlayAddress | `docs/agents/libp2p-networking.md` |
| Wasm client goal, cfg-gating, crate boundary, runtime/transport/storage plan | `docs/agents/wasm.md` |
| API surfaces: FFI primary, gRPC for ops, wasm-bindgen for browsers, no JSON | `docs/agents/api-surface.md` |

Per-area `AGENTS.md` files apply when you edit code in that directory.

| Path | Scope |
|---|---|
| `docs/AGENTS.md` | Prose docs under `docs/`. |
| `bin/vertex/AGENTS.md` | The shipped binary. |
| `crates/net/AGENTS.md` | Protocol-agnostic netutils. |
| `crates/swarm/AGENTS.md` | Swarm domain crates and the libp2p boundary. |
| `crates/swarm/net/AGENTS.md` | `/swarm/...` wire protocols. |
| `crates/swarm/stream/AGENTS.md` | Transport-agnostic bulk get/put streaming combinator. |
| `crates/storage/AGENTS.md` | Storage abstraction and redb backend. |
| `crates/node/AGENTS.md` | Protocol-agnostic node infrastructure. |
| `crates/observability/AGENTS.md` | Logging, tracing, metrics infra. |
| `crates/ffi/AGENTS.md` | Native FFI surface for embedding a client. |

## Doc map

Primary sources for the Process step:

- `docs/swarm/reference/book-of-swarm.txt` (Viktor Tron): conceptual source of truth. Chapter anchors in `docs/agents/swarm-protocol.md`.
- `docs/architecture/overview.md`: layering, dependency direction, libp2p boundary.
- `docs/client/architecture.md`: the libp2p boundary in detail.
- `docs/swarm/protocols.md`: headered streams and per-protocol IDs.
- `docs/swarm/differences-from-bee.md`: deliberate divergences.
- `docs/swarm/hive-gossip.md`: peer discovery gossip.
- `docs/protocol-errors.md`: error taxonomy, `IntoStaticStr` for metric labels.
- `docs/development/bee-protocol-improvements.md`: upstream suggestions, do not unilaterally apply.
- `docs/design/chunk-size-const-generic.md`: the const-generic design template.
- `docs/observability/{design,helpers,profiling}.md`.
- `docs/networking/{address-management,peer-management,peer-dialing-strategy}.md`.
- `docs/cli/configuration.md`.

## Commits, PRs, CI

- Conventional Commits, imperative mood. Scope by area: `feat(swarm-net-pushsync): ...`, `fix(topology): ...`, `chore(deps): ...`, `test(swarm-peer): ...`.
- No em-dashes in commits or PR bodies. No attribution or robot footers in commit messages.
- Read the org guide `github.com/nxm-rs/.github` `CONTRIBUTING.md` before opening any PR. It binds every nxm-rs repo: Oxford English (British vocabulary with `-ize` endings), one PR does one thing, link an issue, and a mandatory `AI Assistance: <tool> used for <parts>` disclosure. The PR body must cover What, Why (the linked issue), Testing, and that disclosure.
- PR bodies are markdown: no hard-wrapped paragraphs. One logical line per paragraph. Let GitHub reflow.
- After every `git push`, run `gh pr checks <N>` and watch until green.
- Destructive operations (`git push --force` to a shared branch, `git reset --hard`, deleting branches): confirm with the human owner first.

## Project tension

Vertex must experiment with the Swarm protocol while shipping a v1 conformant enough to acquire real users on the live network. The two coexist by locking v1 wire behaviour to the reference implementation (see `docs/agents/swarm-protocol.md`) and gating protocol experiments behind `SwarmHardfork` variants selected by `ForkDigest` at handshake time. If you want to "fix" a wire-level quirk in the reference without a fork, you are about to break interop.
