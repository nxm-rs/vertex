# AGENTS.md

Canonical contract for any agent working in this repository (Claude Code, Codex, Cursor, OpenHands, or a human collaborator). `CLAUDE.md` at the same level is a symlink to this file; the same pattern is used inside subdirectories that ship their own per-area `AGENTS.md`.

Vertex is a Rust implementation of the Ethereum Swarm node, designed for modularity, performance, and client diversity. The dominant peer on the live network is the Go bee node; v1 conformance with its wire bytes is required so Vertex can acquire real users, while the internal architecture is free to be idiomatic Rust.

## Process: start every task here

Before you write code, walk this checklist. Skip it only for typo and clippy-lint changes.

1. **Classify the change.** Wire-visible? Public-trait change? Internal refactor? New feature? Bug fix? Each path has different rules below.
2. **Read the relevant guidance.**
   - This file, top to bottom.
   - The per-area `AGENTS.md` for every directory you will touch.
   - The matching deep guide under `docs/agents/` (table below) for any wire, Rust-architecture, or libp2p question.
   - The relevant pages under `docs/` linked from the area files.
   - For protocol semantics: the relevant chapter of `docs/swarm/reference/book-of-swarm.txt`.
3. **Refine the scope and spec before code.**
   - Wire-visible change: define the exact bytes and gate behind a `SwarmHardfork` if it diverges from the reference. Add or update the conformance vectors under the protocol crate's `tests/`.
   - Public-trait change: write or update the design note (crate root rustdoc or `docs/design/`) and run it past the affected crates.
   - New CLI flag or config knob: place it where the per-area `AGENTS.md` for `crates/node/` says it belongs.
4. **Update AGENTS.md before implementing.** If, while reading, you find the guidance for the area is missing, stale, or wrong, fix it in the same PR. Do not implement against guidance you know is wrong and leave it for the next agent.
5. **Implement, then verify.** `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test -p <crate>`. Push. Watch `gh pr checks <N>` until green.

## Top of mind

Rules that catch the most review comments. None of these bend.

- **`multiaddrs`, never `underlay`.** Bee jargon stays in bee. Applies to code, comments, docs, commit messages, PR bodies.
- **No em-dashes.** ASCII hyphens or split the sentence. Source, rustdoc, markdown, commits, PR bodies, chat output.
- **No inline references to the reference implementation in code or operator-facing docs.** Brief architectural notes belong only at the crate root rustdoc, not scattered through call sites. Agent-only files under `docs/agents/` are the exception, since their job is to talk about it.
- **No "Unit N" internal plan labels in shipped rustdoc.** Describe consumers and components by name.
- **Pre-commit is required, not polish.** `cargo fmt --all` and `cargo clippy --all-targets --all-features -- -D warnings`. Zero tolerance for unformatted or warning-bearing pushes.
- **`git push` and `gh pr checks <N>` are one unit.** Watch CI until green. `MERGEABLE` is not the success signal.
- **No Claude attribution in commit messages or PR bodies.** No "Co-Authored-By: Claude", no robot footer.
- **No wire change without a fork gate.** Use `SwarmHardfork` and `ForkDigest`. Never feature-flag wire bytes with cargo features.
- **Primitives and layer-2 constructs live in `nectar`, not `vertex`.** See the Repo split section below before adding chunk, addressing, manifest, feed, BMT, postage, or other domain-primitive code here.
- **Reach for the workspace derive macros before hand-rolling impls.** `thiserror`, `strum`, `derive_more`, `auto_impl(&, Arc, Box)`. Rules in `docs/agents/rust-idiomatic.md`.
- **A client node will run in wasm.** Plan every new crate for the `wasm32-unknown-unknown` target: pick `target_arch` vs feature cfg per `docs/agents/wasm.md`, audit tokio features, keep the wasm cone clean.
- **Public APIs are FFI and gRPC only. No HTTP+JSON.** Vertex is library-first: embedded via FFI (Dart bindings and similar) for native and mobile, gRPC for desktop and server operator scripting, wasm-bindgen for browsers. No `openapi.yml`, no `serde_json` in public paths, no HTTP handler frameworks. Rules in `docs/agents/api-surface.md`.

## Repo split: vertex vs nectar

Vertex owns the **node**: libp2p protocols, peer management, topology, storage backend, observability, CLI, and the binary itself. `nectar` (https://github.com/nxm-rs/nectar) owns the **primitives and layer-2 constructs**: chunks, addressing, BMT hashing, postage stamps and batches, mantaray manifests, feeds, single-owner chunks, and anything else that another Swarm consumer (light client, indexer, web tool, contract verifier) would want to use without pulling in a libp2p stack.

Both repos are under nxm-rs control, so moving code across the boundary is a same-org PR, not an external negotiation.

Belongs in `nectar`:

- Chunk types (`CAC`, `SOC`), span encoding, BMT hash and proofs.
- Address types (`SwarmAddress`, `OverlayAddress` derivation), proximity order, bin math.
- Manifests: mantaray nodes, traversal, edge encoding.
- Feeds: epoch grid, feed lookup, SOC-based mutability.
- Postage: batch contract decode, stamp signing and verification, bucket math.
- Erasure coding, redundancy, recovery primitives.
- Any pure-data validation that does not require a network or a database.

Belongs in `vertex`:

- libp2p `NetworkBehaviour`s and wire protocols.
- Peer manager, topology, scoring, backoff, dialer.
- Storage abstractions (`vertex-storage`) and backends (`vertex-storage-redb`).
- Storer reserve, chunk store, redistribution agent.
- Node lifecycle, builder, CLI, observability, RPC.

How to apply this:

- Before adding a new type or function to a `vertex-swarm-*` crate, ask: would a non-node consumer want this? If yes, draft it in `nectar` (file a PR under `nxm-rs/nectar`) and depend on it from vertex.
- If you find primitive-shaped code already in vertex that belongs upstream, open an issue in this repo and a migration PR in nectar. The workspace pins all nectar deps to the same git rev (`Cargo.toml`) so the move is one rev bump here once nectar merges.
- `vertex-swarm-primitives` is the canonical re-export surface. New nectar exports flow into the rest of the workspace through it, so consumers only see one path.
- If something is genuinely vertex-only (a `Validated*` wrapper that depends on a vertex storage trait, for example), it stays here and the comment at the top of the type says why.

## Build, test, lint

- Edition `2024`, MSRV `1.91`. Do not raise MSRV without bumping the workspace `Cargo.toml` in the same commit.
- `cargo build --release -p vertex` builds the binary into `target/release/vertex`.
- `cargo test` runs workspace unit tests. Per-crate: `cargo test -p <crate>`. Integration tests live under each crate's `tests/`.
- `cargo fmt --all` formats. `cargo clippy --all-targets --all-features -- -D warnings` lints. Both are required pre-commit.
- The `justfile` at repo root collects common workflows. When in doubt, read it.
- Missing tooling on this NixOS host: use `nix-shell -p <pkg> --run "..."`. The project shell is in `flake.nix`.

## Where rules live

Deep prescriptive guidance lives in dedicated documents so this file stays focused.

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
| `crates/storage/AGENTS.md` | Storage abstraction and redb backend. |
| `crates/node/AGENTS.md` | Protocol-agnostic node infrastructure. |
| `crates/observability/AGENTS.md` | Logging, tracing, metrics infra. |

## Doc map

Primary sources to consult during the Process step:

- `docs/swarm/reference/book-of-swarm.txt` (Viktor Tron): conceptual source of truth. Chapter anchors in `docs/agents/swarm-protocol.md`.
- `docs/architecture/overview.md`: layering, dependency direction, libp2p boundary.
- `docs/client/architecture.md`: the libp2p boundary in detail.
- `docs/swarm/protocols.md`: headered streams and per-protocol IDs.
- `docs/swarm/differences-from-bee.md`: deliberate divergences.
- `docs/swarm/hive-gossip.md`: peer discovery gossip.
- `docs/protocol-errors.md`: error taxonomy and `IntoStaticStr` for metric labels.
- `docs/development/bee-protocol-improvements.md`: upstream suggestions, do not unilaterally apply.
- `docs/design/chunk-size-const-generic.md`: the const-generic design template.
- `docs/observability/{design,helpers,profiling}.md`: span boundaries, label values, profiling.
- `docs/networking/{address-management,peer-management,peer-dialing-strategy}.md`.
- `docs/cli/configuration.md`.

## Commits, PRs, CI

- Conventional Commits, imperative mood. Scope by area: `feat(swarm-net-pushsync): ...`, `fix(topology): ...`, `chore(deps): ...`, `test(swarm-peer): ...`.
- No em-dashes, no Claude attribution, no robot footers in commit messages or PR bodies.
- PR bodies are markdown: no hard-wrapped paragraphs. One logical line per paragraph. Let GitHub reflow.
- After every `git push`, run `gh pr checks <N>` and watch until green.
- Destructive operations (`git push --force` to a shared branch, `git reset --hard`, deleting branches): confirm with the human owner first.

## Project tension

Vertex needs to be flexible enough to experiment with the Swarm protocol while shipping a v1 conformant enough to acquire real users on the live network. The way these coexist: v1 wire behaviour is locked to the reference implementation (see `docs/agents/swarm-protocol.md`), and protocol experiments are gated behind `SwarmHardfork` variants and selected by `ForkDigest` at handshake time. If you find yourself wanting to "fix" a wire-level quirk in the reference without a fork, you are about to break interop.
