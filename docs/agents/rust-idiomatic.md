## Rust idiomatic guidance

This codebase targets a serious P2P client; treat it like `sigp/lighthouse` and `libp2p/rust-libp2p`, not like ethersphere/bee. Idiomatic Rust over translated Go. Every rule below is mandatory unless the file's rustdoc says otherwise.

### Toolchain and pre-commit

- Edition `2024`, MSRV `1.92` (set in workspace `Cargo.toml`). Do not introduce features that raise MSRV without a workspace bump in the same commit.
- Before every commit, run `cargo fmt --all` and `cargo clippy --all-targets --all-features -- -D warnings`. Pushing unformatted code or clippy warnings is a hard fail; treat the justfile/CI as the source of truth, not your editor.
- New crates inherit `[workspace.lints]` and use `#![cfg_attr(not(test), warn(unused_crate_dependencies))]` at the crate root (see `crates/tasks/src/lib.rs`).

### Error model

- One flat `thiserror::Error` enum per protocol/crate. No struct errors except for trivial markers like `NoCurrentTaskExecutorError` (zero-field, `#[error("...")]`, `#[non_exhaustive]`).
- Every error enum derives `strum::IntoStaticStr` with `#[strum(serialize_all = "snake_case")]`. Variants that wrap upstream errors carry `#[strum(serialize = "...")]` for stable metric labels and `#[from]` for the conversion. See `crates/swarm/net/handshake/src/error.rs` as the canonical shape.
- No `Protocol(String)`, no `Other(String)`, no `anyhow::Error` inside library crates. Application binaries may use `eyre` at the top level only (see `bin/vertex/src/main.rs`).
- Read `docs/protocol-errors.md` before adding or renaming an error variant; it defines the lifecycle/validation/infrastructure taxonomy and the `LabelValue` blanket impl that makes metrics free.
- Errors flow with `?` and `#[from]`. Do not write `match err { ... return Err(MyError::Foo(format!(...))) }` adapter code; add a `From` impl or a new variant.

### Async patterns

- Tokio multi-thread runtime, built once in `main` (see `bin/vertex/src/main.rs`). Library crates never call `Runtime::new`; they take a `Handle` or a `TaskExecutor`.
- Spawn through `vertex_tasks::TaskExecutor`, not raw `tokio::spawn`. Use `spawn_critical` for must-not-panic loops, `spawn_critical_with_graceful_shutdown_signal` for services that need to drain, `spawn_service` for any `SpawnableTask`. Naked `tokio::spawn` in a library crate gets rejected in review.
- Every spawned future has a defined termination: a `GracefulShutdown` await, a `select!` arm on `on_shutdown`, or a finite stream. No detached "fire and forget" tasks.
- Prefer concrete `impl Future` returns. Use `async fn` in traits when the trait is internal; reserve `BoxFuture` / `Pin<Box<dyn Future>>` for FFI-style boundaries like `TaskSpawner` and `NodeTaskFn` where erasure is genuinely needed.
- `#[async_trait]` is required exactly when the trait is dispatched as a trait object (`Box<dyn T>`, `Arc<dyn T>`), because native `async fn` in trait is not object-safe; pair it with `auto_impl(&, Arc, Box)` so smart-pointer holders get forwarding impls for free (`SwarmSettlementProvider` in `vertex-swarm-api` is the model). A trait only ever consumed through concrete types or generics drops the macro and declares `fn ... -> impl Future<Output = ...> + Send` (or plain `async fn` if no future crosses a spawn boundary); implementors still write `async fn`. The explicit `+ Send` matters: native `async fn` in trait gives callers no way to require `Send` futures, so any trait whose futures cross `tokio::spawn` or `TaskExecutor` boundaries must either spell out the `Send` bound in the return type or keep `#[async_trait]`.
- Drive `libp2p::NetworkBehaviour` state machines with `poll`; do not bolt a `tokio::spawn` onto a behaviour to "simplify" it. Channels owned by the behaviour are size 0 or 1 unless a comment justifies the buffer.

### Derive macros: the workspace dialect

Reach for the workspace derive macros before writing impls by hand. All four are already in the workspace dependency table; use them.

- `thiserror::Error` for every error enum. Flat enums, `#[from]` for infra wrappers, `#[error("...")]` per variant. Covered in the error-model section above.
- `strum::IntoStaticStr` on every error and event enum. With `#[strum(serialize_all = "snake_case")]` at the enum level and `#[strum(serialize = "...")]` overrides on variants that wrap an upstream type, this gives metric labels for free via the `LabelValue` blanket impl in `vertex-metrics` (re-exported from `vertex-observability`).
- `strum::Display` for enums that need a human or log-friendly string. `strum::EnumString` for the parse-from-string case (CLI args, config). `strum::FromRepr` for fixed-discriminant enums (see `SwarmNodeType`).
- `auto_impl::auto_impl(&, Arc, Box)` on every trait whose methods are `&self` and whose consumers will hold the trait through a smart pointer. Established models: `vertex-swarm-api`'s `SwarmIdentity` (`crates/swarm/api/src/identity.rs`), `SwarmConfig` (`crates/swarm/api/src/config.rs`), `SwarmSpec` (`crates/swarm/api/src/swarm.rs`), `GrpcServiceProvider` (`crates/swarm/api/src/providers.rs`). Add it whenever you write a new trait that will be stored as `Arc<dyn T>` so callers do not need to write the forwarding impls by hand.
- `derive_more` (workspace dep, `default-features = false`, `features = ["full"]`) is the right tool for newtype boilerplate: `Deref`, `DerefMut`, `From`, `Into`, `AsRef`, `Display`, `Add`, `Sum`, `Constructor`. Use it on newtypes wrapping a primitive (accounting-unit balances, fixed-width identifiers) instead of hand-rolling `impl From<u64>` and friends. Prefer the focused `From`, `Into`, `AsRef` derives over the catch-all so the public surface stays predictable.

Order of preference when reaching for a derive: `thiserror` then `strum` then `derive_more` then `auto_impl`. They compose; a single trait or enum can derive across all four.

### Type-state and typed APIs

- Prefer enum dispatch (`AdmissionDecision`, `TopologyEvent`, `AdmissionRejection`) over `Box<dyn Trait>` for closed sets. Exhaustive `match` is the contract.
- Use sealed traits (`mod private { pub trait Sealed {} }`) for extension points the workspace controls but third parties must not implement.
- Push invariants into the type system with const generics where the dimension is known at compile time. The chunk-size const-generic plan in `docs/design/chunk-size-const-generic.md` is the model: a single `const BODY_SIZE: usize` flows from `ChunkTypeSet` through `AnyChunk` to `SwarmSpec`. Add new compile-time dimensions the same way; do not pass them as runtime `usize` fields.
- Newtypes for protocol identifiers (`OverlayAddress`, `SwarmAddress`). Do not pass raw `[u8; 32]` across module boundaries.

### Module discipline

- Crate root (`lib.rs`) carries the architectural rustdoc paragraph (see `crates/swarm/api/src/lib.rs`, `crates/swarm/topology/src/lib.rs`) and the only `pub use` re-exports. Leaf modules are `mod foo;` private and re-exported by name from the root.
- A `mod.rs` may have a narrow overview of what its submodules cover. Do not re-export from a leaf `mod.rs` what the crate root already re-exports.
- Do not re-export third-party types from your crate's API unless the consumer genuinely needs to name them.

### Anti-Go-ism checklist

Refuse to write any of these in a review of your own code:

- Callback iteration (`fn for_each(&self, f: impl FnMut(&Peer))`). Return `impl Iterator` or `impl Stream`.
- String errors, sentinel constants, or `if err.to_string().contains("...")`. Match on enum variants.
- `Option<()>` where a `bool` is what you mean. `Result<(), Error>` where a `bool` would do.
- "Helper", "Util", "Manager" types with no state and no invariants. Free functions in the relevant module, or methods on the type they operate on.
- Goroutine-style "spawn a loop somewhere and forget about it". Every loop has an owner, a shutdown path, and a panic boundary.
- Interface-spam: a trait per function. A trait exists to abstract over multiple implementations or to seal an extension point, not to mock for tests.

### Reference patterns to copy

- Lighthouse splits `BeaconChain` into focused subsystems (`Engine`, `Operations`, `Validator`); follow the same fan-out in `crates/swarm/api` where `SwarmPrimitives` -> `SwarmNetworkTypes` -> `SwarmClientTypes` -> `SwarmStorerTypes` add capabilities one trait at a time.
- `rust-libp2p`'s `NetworkBehaviour` style: `poll` returns `ToSwarm` events, no internal `spawn`. `TopologyBehaviour` follows this; new behaviours must too.
- `rust-libp2p`'s codec pattern (`asynchronous_codec` + length-prefixed framing) is the only acceptable wire codec shape. Do not roll a hand-written `AsyncRead` loop.

### Performance hygiene

- No `.clone()` on hot paths for `Vec`, `String`, `HashMap`, or large structs. Borrow, or wrap in `Arc` once at construction.
- Wire payloads use `bytes::Bytes` / `BytesMut`. Do not convert to `Vec<u8>` to "make types easier".
- `Arc` is cloned at handle creation, not per call. Pass `&Handle` where possible.
- Prefer thin handle types (`TopologyHandle`, `TaskExecutor`) over passing whole subsystems by reference.
- Profile before optimizing; `heap.svg` at the repo root shows we already do this.

### Testing

- Unit tests live next to the code in `#[cfg(test)] mod tests`. Integration tests live in `crates/<name>/tests/` (see `crates/swarm/node/tests`, `crates/swarm/peers/peer/tests`).
- Use `vertex-swarm-test-utils` for cluster, identity, peer, spec, and topology fixtures. Do not reinvent these in a downstream crate's `tests/`.
- `proptest` for codec round-trips and any validation function with a non-trivial input space (`crates/swarm/net/handshake` is the model).
- No `#[ignore]` tests on `main`. If a test is flaky, fix it or delete it.
- Tests assert on enum variants (`matches!(err, HandshakeError::NetworkIdMismatch)`), never on `err.to_string()`.

### Documentation

Terse by default. Calibrate low. The code carries the "what"; rustdoc carries the intent and the one non-obvious invariant a reader genuinely needs, and nothing else. PR #412 cut the #350 stack from essay-level verbosity to roughly a fifth: that is the bar. When unsure, cut.

- Crate root (`lib.rs`) carries a short architectural paragraph: what the crate owns, what it does not, and how it connects to neighbours. `crates/swarm/api/src/lib.rs` is the bar. Short, not an essay.
- Module docs (`//!`) state what the module is plus the single load-bearing invariant. No multi-section essays: no `# What this decides`, `# The rule`, `# Why X`, `# Notes` scaffolding. A few lines, not a page. The stamp-index arbiter went from a 48-line module essay to 15 lines in #412 without losing a thing.
- Item docs (`///`): write one only where the contract is not obvious from the signature. Delete docs that restate the signature: `fn new(batch_id, stamp_index)` does not need "Construct from its batch and stamp index". Exception: crates with `#![warn(missing_docs)]` (for example `vertex-swarm-api`, `vertex-node-api`) need a terse one-line doc on every `pub` item, so write the one line, not a paragraph.
- Inline `//` comments only where the next line cannot speak for itself: a byte or on-disk layout, a non-obvious ordering or memory-ordering reason, an infallibility or safety note, a consensus-observable boundary. Delete comments that narrate what the code plainly does. State a given invariant once, not three times.
- Keep, terse: wire and on-disk layouts, consensus-observable rules, genuine safety or ordering rationale, and `# Safety` / `# Errors` / `# Panics` sections that carry real information.
- Do not reference internal plan labels in shipped docs (no "Unit N", "PR-D", "the design gate"). Describe consumers and components by name.
- Do not reference bee inline. Architectural comparisons, if genuinely needed, belong in a single paragraph at the crate root, not scattered through call sites.
- No em-dashes in code, rustdoc, commit messages, PR bodies, or review comments. Use ASCII hyphens or split the sentence.
- Oxford English in all docs and comments: British vocabulary (behaviour, colour, centre) with `-ize` endings (organize, serialize, initialize), per the org `CONTRIBUTING.md`.
