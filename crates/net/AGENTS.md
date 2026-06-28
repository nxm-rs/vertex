# AGENTS: crates/net/

Protocol-agnostic networking utilities: primitives any libp2p protocol can compose. dnsaddr resolution, address scope classification, peer persistence and scoring, per-peer dial tracking, GCRA rate limiting, protobuf framing.

Global rules: see root `/AGENTS.md`. The notes below are the area-specific overlay.

## Scope

- `dnsaddr`, `local`, `utils`: address handling, IP classification, dial eligibility. `local` owns `DialCapability` (IP family x transport suite): `TransportCapability::platform()` mirrors the swarm assembly in `vertex-swarm-node` (TCP natively, secure websockets on wasm32), and `LocalCapabilities::dial_only()` pins the IP half to dual-stack for nodes that never listen. See `docs/networking/address-management.md`.
- `dnsaddr-doh`: wasm-only dnsaddr resolution over DNS-over-HTTPS for browser clients with no raw DNS TXT capability. The recursion driver is generic over a `TxtFetcher`, so parsing, bounded-depth recursion, and wss-leaf filtering test natively against fixtures; the `fetch`-backed `DohClient` is `cfg(target_arch = "wasm32")`. Returns dialable leaves only and stays free of `crates/swarm/` types: the snapshot-fallback policy lives at the caller site (`resolve_or_fallback` takes a caller-supplied snapshot slice). See `docs/agents/wasm.md`.
- `peer/backoff`, `peer/score`, `peer/store`, `peer/registry`: peer state primitives with no protocol logic. With `local` they sit in the wasm compilation cone of the Swarm peer stack: keep them building for `wasm32-unknown-unknown` (the `wasm` CI job enforces this through `vertex-swarm-peer-manager`) and take wall and monotonic clocks from `web-time`, not `std::time`. See `docs/agents/wasm.md`.
- `dialer`: generic dial-request tracker with bounded queue and in-flight management.
- `codec`: protobuf framing (`FramedProto`) plus the `protocol_error!` macro for protocol-shaped error enums.
- `ratelimiter`: single-bucket and keyed GCRA limiters for handlers, plus the outbound `SelfRateLimiter` (dual of the inbound `KeyedRateLimiter`: parks rather than refuses). Keyed buckets carry per-key quotas (`set_key_quota` / `SelfRateLimiter::set_quota`) so an outbound throttle resizes one peer's bucket from a per-peer signal without disturbing the others; a resize is idempotent on an unchanged quota and re-clamps outstanding credit on a shrink.

## libp2p dependency

Crates here may depend on `libp2p` types (`Multiaddr`, `PeerId`, `ConnectionId`): they are network vocabulary. They must not depend on any `NetworkBehaviour`, on `libp2p::Swarm`, or on any `/swarm/...` protocol crate. If you reach for a behaviour, the code belongs in `crates/swarm/net/` or `crates/swarm/`.

## Dos

- Implement error enums with `thiserror` and derive `strum::IntoStaticStr` so the `reason` label round-trips into metrics with no manual mapping.
- Keep types generic over the peer ID and record where possible (for example `PeerSnapshotStore<R>`), so consumers pick the concrete identifier.
- Prefer atomics for hot-path peer counters (`PeerBackoff`, `PeerScore`).
- Add doctest examples for any new public algorithm. The GCRA module is the standard to imitate.

## Donts

- Do not import `nectar-primitives` or anything from `crates/swarm/`, and do not reach across to `crates/swarm/net/*` protocols.
- Do not bake a libp2p `StreamProtocol` into a generic util.
- Do not log inside a hot rate-limiter path. Surface decisions through the return type and let the caller record them.

## Tests

- Per-crate `cargo test -p vertex-net-<name>`. Most crates are pure and run fast without features.
- Workspace-wide: `just test` or `just nextest`. Run `just clippy` before pushing.
