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
| `handshake_decode` | `decode_syn`/`decode_ack`/`decode_synack` on adversarial field content | the multiaddr block decode and its count cap, overlay/nonce/signature length checks, EIP-191 signature recovery, the network-id gate, the welcome cap, and the admission checks reachable from decode never panic |
| `swarm_peer_parse` | `deserialize_multiaddrs` on raw bytes, `SwarmPeer::parse` and the gossip `check_timestamp` policy on adversarial content | the hand-rolled list/uvarint decoder and its count cap, EIP-191 recovery, overlay validation, the chequebook length check, the timestamp and clock-skew checks, and the timestamp policy's accept bounds never panic |
| `hive_decode` | the hive `Peers` batch through `validate_batch` (length checks, the two-tier cache, EIP-191 recovery, the /p2p/ requirement) | validation classifies every raw record exactly once, full-validation successes stay proportional to wire bytes (so a frame-capped message bounds per-message ECDSA work), and re-validating survivors through the cache reproduces them |
| `identify_decode` | the identify message conversions (`Info`/`PushInfo`) over the vendored generated struct | public-key decode, multiaddr and protocol filtering, and the signed-peer-record consistency gate never panic; the push conversion is total |
| `pullsync_decode` | the pullsync message set (`Syn`/`Ack`/`Get`/`Offer`/`Want`/`Delivery`) deserialize + `from_proto` | the bin range on `Get`, the 32-byte descriptor field checks, the implicit bitvector sizing on `Want`, and the stamp parsing plus chunk reconstruction on `Delivery` never panic |
| `pullsync_bitvector` | `check_bitvector` over `BitVector::from_wire_bytes` and `BitVector::from_bytes` | `from_bytes` accepts exactly `len / 8 + 1` bytes, trailing pad bits never surface through `get`/`count_ones`, `set` past `len` is a no-op, and accepted bytes round-trip unchanged |
| `retrieval_decode` | the retrieval `Request` and the address-parameterized `Delivery` decode; a delivery input's first 32 bytes are the requested address, the rest the raw frame, so the fuzzer controls both sides of the chunk reconstruction | the 32-byte address check on `Request`, the empty-data failure sentinel, the optional stamp parse, and the chunk reconstruction against the requested address never panic; a decoded success carries the requested address |
| `headers_decode` | the stream-headers envelope (the exchange every headered protocol negotiation runs) through `check_headers` on writer-produced frames with adversarial entries, plus the flat `Header` entry on raw bytes | the map never exceeds the wire entry count, a duplicate key collapses to its last occurrence (proto3 last-field-wins), and re-encoding the decoded envelope decodes back equal |

The flat frames are fed raw `&[u8]` straight through the generated reader;
the nested frames (the handshake `Ack`/`SynAck`, the pullsync `Offer`, the
headers envelope) go through the real write then read then decode path with
adversarial field content, so the domain decode sees hostile input while the
reader stays on writer-produced bytes. The generated protobuf reader on raw bytes is
upstream code and a separate nested-length soundness issue in
`quick-protobuf` is tracked out of band.

`handshake_uvarint_differential` is not a decode target: it checks the
hand-rolled frame-sizing arithmetic (`uvarint_len`, `entry_len`,
`bound_advertised` in the handshake crate's `address.rs`) against the codec's
actual encoded length, asserts `bound_advertised` never busts the frame budget
(`MAX_HANDSHAKE_BUFFER_SIZE`), and relies on release `overflow-checks` so no
input overflows the budget math.

| Target | Invariant |
|---|---|
| `handshake_uvarint_differential` | predicted block size equals the encoded length, the bound never exceeds the frame budget, and the budget math never overflows |

Round-trip targets take a structured value via the valid-by-construction
`Arbitrary` impls behind each owning crate's `arbitrary` feature (the same
impls drive the stable proptest suites), so the invariant is stronger:
encode must decode back to an equal value.

| Target | Invariant |
|---|---|
| `pushsync_roundtrip` | `from_proto(deserialize(serialize(into_proto(value)))) == value` for deliveries and receipts |
| `handshake_roundtrip` | encode then wire then decode reproduces the syn/ack/synack, signature recovery included |
| `swarm_peer_roundtrip` | a signed record's wire fields parse back to an equal record, and the clock-skew window holds exactly at its inclusive boundaries |
| `pullsync_roundtrip` | `from_proto(deserialize(serialize(into_proto(value)))) == value` across the whole pullsync message set |
| `retrieval_roundtrip` | a request reproduces itself through the wire; a delivery decodes back to its stampless projection (the serve path ships chunk data only) through the address-parameterized codec pair |
| `headers_roundtrip` | `from_proto(deserialize(serialize(into_proto(value)))) == value` for envelopes, whose keys are unique by map construction |

Every decode target has a stable-gated **seed replay test in the library
crate** that pushes the committed seed bytes through the exact same decode
path, so plain `cargo nextest run` proves the seeds stay panic-free without
nightly or libFuzzer:

- `seed_replay_pushsync_decode` in `crates/swarm/net/pushsync/src/codec.rs`
- `seed_replay_handshake_decode` in `crates/swarm/net/handshake/src/codec/mod.rs`
- `seed_replay_swarm_peer_parse` in `crates/swarm/peers/peer/src/serde_multiaddr.rs`
- `seed_replay_hive_decode` in `crates/swarm/net/hive/src/protocol.rs`
- `seed_replay_identify_decode` in `crates/swarm/net/identify/src/protocol.rs`
- `seed_replay_pullsync_decode` in `crates/swarm/net/pullsync/src/codec.rs`
- `seed_replay_pullsync_bitvector` in `crates/swarm/net/pullsync/src/bitvector.rs` (the `pullsync_bitvector` seeds: a two-byte little-endian declared length then the packed bytes, covering truncated and padding-abuse vectors)
- `seed_replay_retrieval_decode` in `crates/swarm/net/retrieval/src/codec.rs` (a delivery seed is the 32-byte requested address followed by the raw wire frame)
- `seed_replay_headers_decode` in `crates/swarm/net/headers/src/codec.rs`

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
