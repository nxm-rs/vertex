# AGENTS: crates/net/

Protocol-agnostic networking utilities. This area provides primitives that any libp2p protocol can compose: dnsaddr resolution, address scope classification, peer persistence and scoring, per-peer dial tracking, GCRA rate limiting, and the protobuf framing codec helpers.

Root-level rules in `/AGENTS.md` apply here too. The notes below are the area-specific overlay.

## Scope

- `dnsaddr`, `local`, `utils`: address handling and IP classification.
- `peer/backoff`, `peer/score`, `peer/store`, `peer/registry`: peer state primitives that hold no protocol logic. Together with `local` they sit in the wasm compilation cone of the Swarm peer stack: keep them building for `wasm32-unknown-unknown` (the `wasm` CI job enforces this through `vertex-swarm-peer-manager`) and take wall clocks and monotonic time from `web-time`, not `std::time`. See `docs/agents/wasm.md`.
- `dialer`: generic dial-request tracker with bounded queue and in-flight management.
- `codec`: protobuf framing (`FramedProto`) plus the `protocol_error!` macro for protocol-shaped error enums.
- `ratelimiter`: single-bucket and keyed GCRA limiters used by handlers.

## libp2p dependency

Crates here may depend on `libp2p` types (`Multiaddr`, `PeerId`, `ConnectionId`) because those are part of the network vocabulary. They must not depend on any `NetworkBehaviour`, on `libp2p::Swarm`, or on any `/swarm/...` protocol crate. If you find yourself reaching for a behaviour, the code belongs in `crates/swarm/net/` or `crates/swarm/`.

## Dos

- Implement error enums with `thiserror` and derive `strum::IntoStaticStr` so the `reason` label round-trips into metrics with no manual mapping.
- Keep types here generic over the peer ID where possible. `NetPeerId` and `NetRecord` exist so consumers pick the concrete identifier.
- Prefer atomics for hot-path peer counters (`PeerBackoff`, `PeerScore`).
- Add doctest examples for any new public algorithm. The GCRA module is the standard to imitate.

## Donts

- Do not import `nectar-primitives` or anything from `crates/swarm/`.
- Do not reach across to `crates/swarm/net/*` protocols.
- Do not name fields or arguments `underlay`. The word here is `multiaddrs`.
- Do not bake a libp2p `StreamProtocol` into a generic util.
- Do not log inside a hot rate-limiter path. Surface decisions through the return type and let the caller record them.

## Tests

- Per-crate `cargo test -p vertex-net-<name>`. Most crates are pure and run fast without features.
- Workspace-wide: `just test` or `just nextest`. Run `just clippy` before pushing.
