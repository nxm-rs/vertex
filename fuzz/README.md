# Fuzzing vertex

[cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) (libFuzzer)
harness for vertex's wire-format decoders: the code that parses untrusted
bytes off the Swarm network. This directory is an **independent cargo
workspace** (it carries its own `[workspace]` table), so the stable toolchain
building the parent workspace never touches the nightly-only fuzz crate.

## Quickstart

```sh
nix develop .#fuzz        # nightly cargo + cargo-fuzz + clang on PATH
cargo fuzz list           # run from the repo root; cargo-fuzz finds fuzz/

# Run a target, growing fuzz/corpus/<target> and merging the committed seeds.
# libFuzzer requires the writable corpus dir (first positional) to exist, and
# it is gitignored, so create it once on a fresh checkout:
mkdir -p fuzz/corpus/pushsync_decode
cargo fuzz run pushsync_decode fuzz/corpus/pushsync_decode fuzz/seeds/pushsync_decode

# Or let cargo-fuzz manage the corpus dir for you (no explicit paths):
cargo fuzz run pushsync_decode -- -max_total_time=60

# Housekeeping:
cargo fuzz cmin pushsync_decode                                   # minimize corpus
cargo fuzz tmin pushsync_decode fuzz/artifacts/pushsync_decode/x  # minimize a crash
cargo fuzz coverage pushsync_decode                               # llvm-cov profdata
```

New corpus entries are written to the **first** directory passed to
`cargo fuzz run`; further directories (the seeds) are read-only inputs.
`cargo fuzz coverage` finds `llvm-profdata`/`llvm-cov` through the rustc
sysroot; the fuzz shell's nightly ships the `llvm-tools` component.

Release-profile `overflow-checks` and `debug-assertions` are enabled in
`Cargo.toml`, so arithmetic overflow and `debug_assert!` violations are fuzz
oracles, not silent wraps.

## Target catalogue

The wire path is quick-protobuf: the generated struct deserializes the raw
bytes, then `ProtoMessage::from_proto` runs the domain validation. The
generated decoder is well exercised upstream; the domain conversion is this
workspace's code and the actual attack surface, so every target drives both
layers.

Decode targets take raw `&[u8]` and a returned `Err` is success; the
invariant is *no panic, no OOM, no hang*:

| Target | Entry point | Invariant |
|---|---|---|
| `pushsync_decode` | `pushsync::{Delivery, Receipt}` deserialize + `from_proto` | address/nonce length checks, stamp parsing, chunk reconstruction, signature parsing, and the storage radius range never panic |

Round-trip targets take a structured value via valid-by-construction
`Arbitrary` inputs, so the invariant is stronger: encode must decode back to
an equal value.

| Target | Invariant |
|---|---|
| `pushsync_roundtrip` | `from_proto(deserialize(serialize(into_proto(value)))) == value` for deliveries and receipts |

Every decode target has a stable-gated **seed replay test in the library
crate** that pushes the committed seed bytes through the exact same decode
path, so plain `cargo nextest run` proves the seeds stay panic-free without
nightly or libFuzzer:

- `seed_replay_pushsync_decode` in `crates/swarm/net/pushsync/src/codec.rs`

The round-trip invariants are pinned on stable by the `assert_proto_roundtrip!`
tests next to each codec.

## Corpus & seed policy

- `fuzz/seeds/<target>/` is **committed**: a small curated set per decode
  target with a few valid encodings, interesting invalid/edge encodings, and
  minimized crash inputs from fixed bugs. Name seeds
  `valid-*`/`invalid-*`/`edge-*`/`crash-*` with a message and size hint.
- `fuzz/corpus/`, `fuzz/artifacts/`, `fuzz/coverage/` are **gitignored**; the
  corpus lives in the CI cache and on developer machines.
- When a fuzzer finds a crash: `cargo fuzz tmin` it, commit the minimized
  bytes as a `crash-*` seed, extend the crate's `seed_replay_*` test, fix the
  bug (fix and seed in the same commit), then `cargo fuzz cmin`.
- `fuzz/Cargo.lock` is committed so CI builds are reproducible.

## CI cadence

`.github/workflows/fuzz.yml` (the existing workflows are untouched):

- **Every PR / push to main**: `fuzz build` compiles all targets (the
  harness can't rot), and `fuzz smoke` runs each target for 60 s
  (`-rss_limit_mb=2048`) on a per-target cached corpus with the committed
  seeds merged in. Crash artefacts are uploaded on failure.
- **Nightly cron**: 10 minutes per target on the same corpus caches,
  followed by `cargo fuzz cmin` so the caches don't grow without bound.

## NixOS gotchas

- Use `nix develop .#fuzz`. `libfuzzer-sys` compiles the libFuzzer C++
  runtime through the `cc` crate, which is why the shell carries `clang`;
  outside the shell the build fails at that step.
- Don't force `lld`/`mold` via `RUSTFLAGS` for fuzz builds: the sanitizer
  runtimes are linked by rustc's defaults and alternative-linker flags are a
  recurring source of broken ASan link steps. Plain defaults work.
- If an ASan-instrumented run dies immediately with an endlessly repeating
  `DEADLYSIGNAL` banner, the kernel's ASLR entropy is too high for ASan's
  shadow mapping (Linux >= 6.5 defaults): `sudo sysctl vm.mmap_rnd_bits=28`.
- `cargo fuzz coverage` needs the toolchain's llvm-tools; the fuzz shell's
  nightly includes the component, so no extra install is needed.
