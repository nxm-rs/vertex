# Turmoil-backed libp2p simulation: feasibility note

## Verdict: go

A `libp2p_core::Transport` over turmoil's simulated TCP works with no fork changes and no production code changes. Real vertex swarms complete the handshake protocol across two simulated hosts, and the seeded event trace is byte-identical across repeated runs, both in-process and across processes.

## What was proven

All claims below are backed by executing tests in `crates/swarm/sim/tests/handshake.rs`.

- **Transport**: `TurmoilTransport` (in `vertex-swarm-sim`) implements `Transport` over `turmoil::net::{TcpListener, TcpStream}`. turmoil's net types implement the tokio I/O traits; a thin wrapper re-exposes them through the futures traits libp2p upgrades expect. The transport lives in this repository, not the libp2p fork: it needs nothing non-public from libp2p, so there is no reason to carry it as fork delta.
- **Round-trip**: two turmoil hosts each drive a real `Swarm<HandshakeBehaviour<Identity, NoAddresses>>` and both sides complete the full syn/synack/ack handshake, over both the plaintext and the production-shaped noise-plus-yamux stack. The whole exchange completes in about 470ms of virtual time and about 50ms of wall time.
- **Virtual time**: libp2p deadlines ride turmoil's clock with no extra wiring. The fork routes all libp2p timers through `libp2p-timer`, which picks the tokio clock whenever a tokio runtime handle exists; turmoil runs each host on a paused current-thread runtime, so handshake timeouts, upgrade timeouts, and idle timeouts are all virtual.
- **Yamux**: negotiates and multiplexes normally under the sim. It arms no wall-clock timers of its own (window updates are data-driven, no keepalive), so it introduced no nondeterminism: the noise-plus-yamux trace was as seed-stable as the plaintext one.
- **Seed stability**: the identical seeded sim run twice in one process produces identical normalized event traces (virtual timestamps, peer ids, addresses, handshake payloads). The same trace also reproduced exactly across separate processes. A different turmoil seed shifts message latencies and produces a different trace, so the seed demonstrably drives the schedule.
- **Entropy-free path**: seeded libp2p keypairs (`test_keypair`) plus a seeded overlay signer (`Identity::new` over a fixed private key) mean the plaintext path draws no OS entropy at all. Noise draws ephemeral X25519 keys from the OS RNG, which varies the wire bytes per run but never fed back into event ordering in these tests; plaintext remains the honest default for byte-level reproducibility claims.

## Remaining nondeterminism sources

None of these broke the two-host runs; all are recorded for the sim layer to manage as scenarios grow.

- **Wall-clock reads**: turmoil virtualizes the tokio clock, not `SystemTime` or `std::time::Instant`. Signed peer records timestamp via `Timestamp::now()` (SystemTime), so wire bytes and record contents vary run to run even though event order does not. The sim layer should either inject a logical wall clock or keep assertions off raw timestamps. Metric latency samples (`vertex-util-runtime` `Instant`) are real time; keep them out of assertions.
- **tokio's thread-local RNG**: unbiased `tokio::select!` randomizes branch polling order from an RNG the sim seed does not cover (seeding the runtime RNG needs `tokio_unstable`). Sim-side driving code must use `biased` selects or single-future drives; production tasks that use unbiased `select!` are a latent ordering source once whole nodes run under the sim.
- **HashMap iteration order**: std `RandomState` differs per map instance. Invisible with one connection; a many-peer sim iterating maps into scheduling decisions can surface it, in vertex code and in libp2p internals alike.
- **FuturesUnordered polling**: deterministic given deterministic insertion and wake order, which the single-threaded sim provides; it inherits any divergence from map-ordered insertion rather than adding its own.
- **Process-global counters**: `ConnectionId` allocates from a process-wide counter, so it is not stable across runs in one process. Exclude such ids from traces; assert on peer ids, overlays, and addresses instead.
- **turmoil constraints worth knowing**: listeners bind only unspecified or loopback addresses; host IPs are assigned in registration order and ephemeral ports count deterministically per host, so host registration order is part of the seed surface.

## SimWorld shape for the next stage

- **Builder**: `SimWorld::builder().seed(u64)` plus latency range, tick, and duration controls mapping onto `turmoil::Builder`; `build()` wraps `turmoil::Sim`. One world per test, run on the current thread.
- **Hosts**: named hosts registered in deterministic order. Each host derives its identities from the world seed plus the host name, wraps a real swarm (or full node) built over `TurmoilTransport`, and defaults to the plaintext stack so the deterministic path stays entropy-free, with noise opt-in for production-shaped runs.
- **Node injection**: whole nodes enter through the existing production seam (`TransportOverride` / `with_transport`), so the sim never needs private constructors. Behaviour-level hosts construct the swarm directly, as the spike test does.
- **Fault controls**: partition, repair, latency reshaping, and host restart re-exposed as world methods over turmoil's; hold and release of individual links gives drop injection.
- **Harness compatibility**: the world exposes the same drive vocabulary as the seeded harness (`drive_until`-style predicates over swarm events) so behaviour tests written against the harness surface can move under the sim without rewriting; transport stays out of test-facing signatures.
- **Trace**: a world-owned event log (host, virtual time, normalized event) with the id-exclusion rules above, so seed-replay assertions are one `assert_eq!` and a printed seed reproduces a failure exactly.
