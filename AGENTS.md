# AGENTS.md

This file is the canonical contract for any agent that works in this repository.
An agent is Claude Code, Codex, Cursor, OpenHands, or a human collaborator.
`CLAUDE.md` at the same level is a symlink to this file.
Subdirectories that ship their own per-area `AGENTS.md` follow the same pattern.

Vertex is a Rust implementation of the Ethereum Swarm node.
Vertex is designed for modularity, performance, and client diversity.
The dominant peer on the live network is the Go reference node.
Vertex must conform to the reference wire bytes for v1 so that Vertex can acquire real users.
The internal architecture is free to be idiomatic Rust.

## Process: start every task here

Walk this checklist before you write code.
Skip the checklist only for typo and clippy-lint changes.

1. **Classify the change.**
   Decide whether the change is wire-visible, public-trait, an internal refactor, a new feature, or a bug fix.
   Each path has different rules below.
2. **Read the relevant guidance.**
   Read this file from top to bottom.
   Read the per-area `AGENTS.md` for every directory you touch.
   Read the matching deep guide under `docs/agents/` in the table below for any wire, Rust-architecture, or libp2p question.
   Read the `docs/` pages that the area files link.
   For protocol semantics, read the relevant chapter of `docs/swarm/reference/book-of-swarm.txt`.
3. **Refine the scope and spec before you write code.**
   - Wire-visible change: define the exact bytes.
     Gate the change behind a `SwarmHardfork` if it diverges from the reference.
     Add or update the conformance vectors under the protocol crate's `tests/`.
   - Public-trait change: write or update the design note in the crate root rustdoc or `docs/design/`.
     Run the design note past the affected crates.
   - New CLI flag or config knob: place it where `crates/node/AGENTS.md` says it belongs.
4. **Update AGENTS.md before you implement.**
   If guidance for an area is missing, stale, or wrong, fix it in the same PR.
   Do not implement against guidance that you know is wrong.
5. **Implement, then verify.**
   Run `cargo fmt --all`, then `cargo clippy --all-targets --all-features -- -D warnings`, then `cargo test -p <crate>`.
   Push.
   Watch `gh pr checks <N>` until green.

## Top of mind

These rules catch the most review comments.
None of these rules bend.

- **Use `multiaddrs` for a peer transport address, never the deprecated synonym.**
  The hook `.claude/hooks/content-lint.sh` enforces this rule.
  It applies to code, comments, docs, commits, and PR bodies.
- **Use no em-dashes.**
  The hook `.claude/hooks/content-lint.sh` enforces this rule.
  Use ASCII hyphens or split the sentence.
- **Write no inline references to the reference implementation in code or operator-facing docs.**
  Brief architectural notes belong only in the crate root rustdoc, not scattered through call sites.
  The agent-only files under `docs/agents/` are the exception.
- **Write no "Unit N" internal plan labels in shipped rustdoc.**
  Describe consumers and components by name.
- **Keep rustdoc terse by default and calibrate low.**
  State the intent and the one non-obvious invariant that a reader needs: a wire or byte layout, a consensus-observable rule, or a real safety or ordering reason.
  Write no module essays, no `///` that restates the signature, and no `//` that narrates the next line.
  Comment only what the code cannot say, and comment it once.
  Full guidance is in `docs/agents/rust-idiomatic.md`.
- **Treat pre-commit as required, not as polish.**
  Run `cargo fmt --all` and `cargo clippy --all-targets --all-features -- -D warnings`.
  Never push unformatted or warning-bearing code.
- **Scope verification to the change, because CI runs the full matrix.**
  Test the crates you touched with `cargo test -p <crate>`, not the whole workspace.
  Never run benches as a correctness gate or outside performance work.
  A doc-only or comment-only change needs clippy for `missing_docs`, and doctests only if a `///` fence changed, never the test suite.
  For a comment-only restack or a pure move, prove code-equivalence with a filtered `git diff` instead of a recompile.
  Full rules are in `docs/agents/rust-idiomatic.md`.
- **Treat `git push` and `gh pr checks <N>` as one unit.**
  Watch CI until it is green.
  `MERGEABLE` is not the success signal.
- **Add no attribution in commits, and add the required AI disclosure in PR bodies.**
  Keep commit messages clean: no "Co-Authored-By" lines and no robot footer.
  PR bodies must include a factual `AI Assistance: <tool> used for <parts>` line, per the org guide `github.com/nxm-rs/.github` `CONTRIBUTING.md`.
  If you omit the disclosure, you risk PR closure or a ban.
- **Make no wire change without a fork gate.**
  Use `SwarmHardfork` and `ForkDigest`.
  Never feature-flag wire bytes with cargo features.
- **Keep primitives and layer-2 constructs in `nectar`, not in `vertex`.**
  Read the Repo split section before you add chunk, addressing, manifest, feed, BMT, postage, or other domain-primitive code here.
- **Reach for the workspace derive macros before you hand-roll an impl.**
  Use `thiserror`, `strum`, `derive_more`, and `auto_impl(&, Arc, Box)`.
  Rules are in `docs/agents/rust-idiomatic.md`.
- **A client node runs in wasm.**
  Plan every new crate for `wasm32-unknown-unknown`.
  Pick `target_arch` cfg or feature cfg per `docs/agents/wasm.md`, audit the tokio features, and keep the wasm cone clean.
- **Public APIs are FFI and gRPC only, with no HTTP and JSON.**
  Vertex is library-first: FFI such as Dart bindings for native and mobile, gRPC for desktop and server operator scripting, and wasm-bindgen for browsers.
  Add no `openapi.yml`, no `serde_json` in public paths, and no HTTP handler frameworks.
  Rules are in `docs/agents/api-surface.md`.

## Repo split: vertex vs nectar

Vertex owns the **node**: libp2p protocols, peer management, topology, storage backend, observability, CLI, and the binary.
`nectar` at https://github.com/nxm-rs/nectar owns the **primitives and layer-2 constructs**: anything another Swarm consumer wants without a libp2p stack.
Another Swarm consumer is a light client, an indexer, a web tool, or a contract verifier.
Both repos are under nxm-rs control, so a move across the boundary is a same-org PR.

These items belong in `nectar`.
Chunk types (`CAC`, `SOC`), span encoding, and BMT hash and proofs.
Address types (`SwarmAddress`, `OverlayAddress` derivation), proximity order, and bin math.
Manifests: mantaray nodes, traversal, and edge encoding.
Feeds: epoch grid, lookup, and SOC-based mutability.
Postage: batch contract decode, stamp signing and verification, and bucket math.
Erasure coding, redundancy, and recovery.
Any pure-data validation that needs neither the network nor the database.

These items belong in `vertex`.
libp2p `NetworkBehaviour`s and wire protocols.
Peer manager, topology, scoring, backoff, and dialer.
Storage abstractions (`vertex-storage`) and backends (`vertex-storage-redb`).
Storer reserve, chunk store, and redistribution agent.
Node lifecycle, builder, CLI, observability, and RPC.

Apply this split as follows.

- Before you add a type or function to a `vertex-swarm-*` crate, ask whether a non-node consumer would want it.
  If yes, draft it in `nectar` as a PR under `nxm-rs/nectar` and depend on it from vertex.
- If you find primitive-shaped code in vertex that belongs upstream, open an issue here and a migration PR in nectar.
  The workspace pins all nectar deps to the same git rev in `Cargo.toml`, so the move is one rev bump here once nectar merges.
- `vertex-swarm-primitives` is the canonical re-export surface.
  New nectar exports flow into the workspace through it, so consumers see one path.
- If something is genuinely vertex-only, it stays here, and a comment at the top of the type says why.
  An example is a `Validated*` wrapper that depends on a vertex storage trait.

## Feature and cfg contract

Vertex ships three artefacts: a bare client as the default, a storer through `--features storer`, and the FFI client library as the `vertex-ffi` cdylib.
The cone guards enforce this split: `just check-cone` and the `features` CI job.

- `default = []` is the bare client and is load-bearing: no storer cone, no chain, and no swap.
  Never write `default = ["..."]` on a shipped crate.
- Features are for capabilities such as `chain`, `swap`, the `storer` composite, and observability slices, never for node types.
  A node type is the runtime `SwarmNodeType`, dispatched at launch, never a per-type feature.
- `#[cfg(feature = ...)]` lives only at the composition roots: `bin/vertex` for the CLI, `vertex-swarm-builder` for launch, `crates/ffi` for the lib, and `vertex-node-builder` for the protocol-agnostic launch shell.
  In `vertex-node-builder`, the optional `metrics` slice gates the Prometheus recorder and the axum server.
  Domain crates (`client-behaviour`, `client-protocol`, `api`, `topology`, and the node protocol) take their capabilities through traits and optional providers and carry no feature cfg, with one sanctioned exception: the `swap` capability.
  Swap cheque variants and their dispatch are gated inside `client-behaviour` and `client-protocol`, because a swap-off build must exclude the swap cone at compile time to stay lean, and a runtime provider cannot reach that cone.
  The swap cone is `vertex-swarm-net-swap`, the swap settlement crates, and the chain provider's swap pulls.
  This is interop-safe: a swap-off client that speaks only pricing and pseudosettle is a normal peer.
  Do not "tidy" these gates away by always-compiling the swap codec, because that drags the swap and chain cone into the bare and wasm client, which the swap cone guard exists to catch.
- Platform boundaries are `target_arch` cfg, never a feature.
  Never combine a feature and a target in one dependency table entry.
- FFI is a crate, the cdylib artefact, not a feature.
  There is no `ffi` feature anywhere, because the crate boundary scopes it.
- A workspace member must not unconditionally enable `chain`, `swap`, or `storer` on a shared crate.
  Cargo unifies features across the build graph, so one such edge pulls the cone into the default client.
  This is the unification footgun that the cone guards catch.

## Build, test, lint

- The edition is `2024` and the MSRV is `1.94`.
  Do not raise the MSRV without a bump to the workspace `Cargo.toml` in the same commit.
- `cargo build --release -p vertex` builds the binary into `target/release/vertex`.
- `cargo nextest run` runs the workspace unit and integration tests.
  Doctests run separately through `cargo test --doc`, because nextest does not run them.
  Run one crate with `cargo nextest run -p <crate>`.
  Integration tests live under each crate's `tests/`.
- `cargo fmt --all` formats the code.
  `cargo clippy --all-targets --all-features -- -D warnings` lints the code.
  Both are required pre-commit.
- The `justfile` at the repo root collects the common workflows.
  When in doubt, read it.
- For missing tooling on this NixOS host, use `nix-shell -p <pkg> --run "..."`.
  The project shell is in `flake.nix`.
- CI builds on the channel that `rust-toolchain.toml` pins, which is the MSRV, and never on current stable.
  Every job takes the toolchain and the cache from the `.github/actions/rust-setup` composite action.
  Keep `RUSTFLAGS` unset and out of the workflows, because a per-job difference re-fingerprints the whole graph and invalidates the restored target directory.
  An sccache layer over the Actions cache was measured and rejected: it costs two to three times the baseline, and the reasons are in the composite action.
  CI passes `--locked` to every workspace cargo call, so a stale `Cargo.lock` fails the run.
  A change that touches only markdown, `docs/`, `.claude/`, or `LICENSE` skips the five compile jobs, because no `.rs` file pulls a markdown file into rustdoc.
- `.claude/` ships Claude Code hooks.
  rustfmt-on-edit formats each file on Write and Edit.
  nextest-on-stop runs `cargo nextest run` for the touched crates.
  content-lint blocks em-dashes and the deprecated peer-address term.
  The shared hook config is tracked, and personal and session state stays ignored.

## Documentation

Write all documentation in ASD-STE100 Simplified Technical English.
Use short sentences, the active voice, and one idea per sentence.
In markdown files, put each sentence on its own line and do not wrap within a sentence, because GitHub reflows the file when it displays it.
This keeps a diff to one changed line per changed sentence.
In PR and issue bodies, keep one line per paragraph, because GitHub renders a single newline in a comment as a line break.

## Where rules live

| Area | File |
|---|---|
| Swarm wire conformance, fork gating, terminology, Book of Swarm anchors | `docs/agents/swarm-protocol.md` |
| Rust idioms, error model, async patterns, anti-Go-isms, testing | `docs/agents/rust-idiomatic.md` |
| libp2p boundary, NetworkBehaviour rules, codecs, PeerId vs OverlayAddress | `docs/agents/libp2p-networking.md` |
| Wasm client goal, cfg-gating, crate boundary, runtime/transport/storage plan | `docs/agents/wasm.md` |
| API surfaces: FFI primary, gRPC for ops, wasm-bindgen for browsers, no JSON | `docs/agents/api-surface.md` |

The per-area `AGENTS.md` files apply when you edit code in that directory.

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

These are the primary sources for the Process step.

- `docs/swarm/reference/book-of-swarm.txt` (Viktor Tron): conceptual source of truth. Chapter anchors in `docs/agents/swarm-protocol.md`.
- `docs/architecture/overview.md`: layering, dependency direction, libp2p boundary.
- `docs/client/architecture.md`: the libp2p boundary in detail.
- `docs/swarm/protocols.md`: headered streams and per-protocol IDs.
- `docs/swarm/differences-from-bee.md`: deliberate divergences.
- `docs/swarm/hive-gossip.md`: peer discovery gossip.
- `docs/protocol-errors.md`: error taxonomy, `IntoStaticStr` for metric labels.
- `docs/development/bee-protocol-improvements.md`: upstream suggestions, do not unilaterally apply.
- `docs/design/chunk-size-const-generic.md`: the const-generic design template.
- `docs/design/accounting-seam.md`: the bandwidth-accounting trait surface (Ledger/AdmissionControl/Reserve/Settle, the Debt newtype, the settlement enum).
- `docs/design/client-credit-admission.md`: the client-side reserve/settle/skip band policy.
- `docs/observability/{design,helpers,profiling}.md`.
- `docs/networking/{address-management,peer-management,peer-dialing-strategy}.md`.
- `docs/cli/configuration.md`.

## Commits, PRs, CI

- Use Conventional Commits in the imperative mood.
  Scope by area: `feat(swarm-net-pushsync): ...`, `fix(topology): ...`, `chore(deps): ...`, and `test(swarm-peer): ...`.
- Use no em-dashes in commits or PR bodies.
  Use no attribution or robot footers in commit messages.
- Read the org guide `github.com/nxm-rs/.github` `CONTRIBUTING.md` before you open any PR.
  It binds every nxm-rs repo: Oxford English with British vocabulary and `-ize` endings, one PR does one thing, link an issue, and a mandatory `AI Assistance: <tool> used for <parts>` disclosure.
  The PR body must cover What, Why with the linked issue, Testing, and that disclosure.
- PR bodies are markdown: use no hard-wrapped paragraphs.
  Use one logical line per paragraph.
  Let GitHub reflow the text.
- After every `git push`, run `gh pr checks <N>` and watch until it is green.
- Confirm destructive operations with the human owner first.
  A destructive operation is `git push --force` to a shared branch, `git reset --hard`, or a branch deletion.

## Project tension

Vertex must experiment with the Swarm protocol and at the same time ship a v1 that is conformant enough to acquire real users on the live network.
The two goals coexist through two rules.
Lock the v1 wire behaviour to the reference implementation, as `docs/agents/swarm-protocol.md` describes.
Gate protocol experiments behind `SwarmHardfork` variants that `ForkDigest` selects at handshake time.
If you want to "fix" a wire-level quirk in the reference without a fork, you are about to break interop.
