# AGENTS: crates/swarm/

Swarm domain layer. Defines what a Swarm node is, the network spec, identity, peer records, bandwidth accounting, peer manager, topology, localstore and storer config, redistribution config, and the RPC service surface. The libp2p-aware composition lives in `swarm/node` and `swarm/topology`; the rest is protocol-agnostic.

Root-level rules in `/AGENTS.md` apply here too. The notes below are the area-specific overlay. The wire-protocol crates under `crates/swarm/net/` have their own `AGENTS.md`.

## Sub-area map

- `primitives`, `forks`, `spec`, `identity`: pure data and configuration. `nectar-primitives` is the canonical source for chunk and address types.
- `peers/peer`: the `SwarmPeer` record with the EIP-191 handshake signature (distinct from postage EIP-191 signing, which lives upstream in `nectar-postage`). See the follow-up tracked at `nxm-rs/vertex` for extracting the sign-data primitive to nectar.
- `peers/peer-manager`, `peers/peer-score`: lifecycle, persistence, scoring. The manager is the authoritative peer hub and holds the ENTIRE known peer set in memory (one `DashMap`; the `ProximityIndex` is a pure bin-membership index with a per-bin admission cap). Persistence is an optional identity-only snapshot (`PeerSnapshot` via the `PeerSnapshotStore` trait from `vertex-net-peer-store`), loaded once at startup and written periodically by `PeerManager::tick` plus a final write on shutdown; reputation, bans, and dial backoff are runtime-only and never survive a restart. The manager owns no timers: a thin `spawn_peer_manager_task` driver is spawned from the node launch path. Subsystems change a peer's score only through `PeerManager::report_peer` (the `PeerReporter` trait from `api`), and the manager broadcasts `PeerLifecycleEvent`s that topology consumes to execute disconnects and bans. `peer-score` is policy-only: `record_event` returns a `ScoreOutcome`, it never executes actions and has no callback hooks.
- `bandwidth/{core,pricing,pseudosettle,swap,chequebook}`: per-peer balances in Accounting Units (AU) and the settlement providers. `Accounting` implements `PeerAffordability` and the accounting and settlement services take an optional `PeerReporter` (both from `api`) so violations feed peer scoring; the node layer does the wiring.
- `api`: the trait chain `SwarmPrimitives` to `SwarmNetworkTypes` to `SwarmClientTypes` to `SwarmStorerTypes`. Strictly libp2p-free with the documented `Multiaddr` exception.
- `builder`: layered builders that produce `BuiltBootnode`, `BuiltClient`, `BuiltStorer`.
- `node`: composes the libp2p `NetworkBehaviour` and exposes `BootNode`, `ClientNode`, `StorerNode`. This is where libp2p shows up.
- `topology`: libp2p behaviour for peer discovery, kademlia routing, reachability tracking.
- `localstore`, `storer`, `redistribution`: storer-side configuration and the chunk-store/reserve implementation.
- `rpc`: tonic-generated gRPC services and a `GrpcServiceProvider` trait.
- `test-utils`: `MockIdentity`, `MockTopology`, and fixtures.

## libp2p dependency

- libp2p-free: `primitives`, `forks`, `spec`, `identity`, `api` (apart from the `Multiaddr` re-export), `builder` (almost), `bandwidth/*`, `localstore`, `redistribution`, `storer`, `rpc`, `test-utils`, `peers/peer-manager`, `peers/peer-score`.
- libp2p-aware: `peers/peer` (uses `Multiaddr`, `PeerId`), `node`, `topology`.

When in doubt: if you need a `NetworkBehaviour` or a `Swarm`, you are in `swarm/node`, `swarm/topology`, or a future composite-behaviour crate for a node type. If you need only `Multiaddr` or `PeerId` you can be elsewhere, but justify it.

### Composite behaviours for node types

`swarm/topology` is the model for a composite `NetworkBehaviour` that owns several sub-protocols (kademlia routing, hive gossip, NAT discovery, reachability). The same pattern is planned for the Client and Storer node types: a single composite behaviour that brings together pricing, swap, pseudosettle, pushsync, retrieval, and similar role-specific protocols, exposed through one type to the libp2p `Swarm` in `swarm/node`.

When such a crate lands (`vertex-swarm-client-behaviour`, `vertex-swarm-storer-behaviour`, or similar), it follows the same rules as `topology`:

- Owns its sub-protocols, composes them with `#[derive(NetworkBehaviour)]`, exposes a single event enum.
- Does not depend on a specific `Swarm` configuration; `swarm/node` selects the composite for the node type.
- Lives in `crates/swarm/` (libp2p-aware tier), not in `crates/swarm/net/` (per-protocol tier).
- Protocol crates under `crates/swarm/net/` stay one-protocol-per-crate. Composition happens here.

## Nectar boundary

Primitives and layer-2 constructs (chunks, addresses, BMT, manifests, feeds, postage, erasure) live in `nectar` (https://github.com/nxm-rs/nectar), not in vertex. Before adding a new type to `primitives`, `spec`, or any sibling crate, check the Repo split section in `/AGENTS.md`. If the new type is non-node-specific, file the PR against `nxm-rs/nectar` and consume it here through `vertex-swarm-primitives` once merged. The workspace pins all nectar deps to the same git rev (`/Cargo.toml`).

## Dos

- Treat `vertex-swarm-api` as the trait surface. New domain capabilities go in an api trait first, then in a concrete implementation crate.
- Use `nectar-primitives` re-exports (`OverlayAddress`, `Bin`, `ProximityOrder`, `Nonce`, `Timestamp`) so the canonical types stay consistent across the workspace.
- Keep the three proximity types distinct; do not collapse them to `u8` (the bee `po: u8` habit). `ProximityOrder` is the metric between two addresses (rank by closeness to a target); `Bin` is a peer's slot index in the local table (keys per-bin storage/iteration); `NeighborhoodDepth` is the boundary bin. Bridges: `Bin::from(po)` is the only `ProximityOrder -> Bin` conversion; `NeighborhoodDepth::{new, bin, contains}` are the only ways in/out of a depth. `NeighborhoodDepth` is intentionally NOT comparable with `Bin` - write `depth.contains(bin)`, never `bin >= depth`. Enumerate bins only via `all_bins`/`balanced_bins(depth)`/`neighborhood_bins(depth, max)`; extract `.get()` only at edges (logs, metrics, wire). Full rationale: `vertex-swarm-primitives` crate docs.
- When `vertex-swarm-primitives` would host a new type that has no node dependency, push it upstream to `nectar` instead and re-export it here.
- Error types are `thiserror` enums with `strum::IntoStaticStr` so they emit a `reason` label cleanly.
- Use accounting units (AU) in bandwidth code. Never mix bytes, BZZ, and AU in the same struct field.
- For node-type capabilities, branch on `SwarmNodeType` from the api crate rather than duplicating the enum.

## Donts

- Do not import `libp2p` in `api`, `spec`, `forks`, `primitives`, `bandwidth/*`, or `localstore`. These are the libp2p-free layer.
- Do not use `underlay` in field names, error messages, or docs. The right word is `multiaddrs`.
- Do not add `serde_json` to a re-enabled crate without a workspace conversation. Several siblings are disabled for exactly that reason. The one settled exception is wire equivalence: a fixed-shape JSON object that must stay byte-identical to the live network (the swap cheque in `bandwidth/chequebook`) may use `serde_json` over a fixed-order wire struct. Tolerate JSON only for that, never for an internal or public API surface.
- Do not write architectural notes about the reference implementation inside crate-level rustdoc. If a comparison helps a reviewer, keep it short and factual, and never imply the reference is canonical.
- Do not store `Arc<Identity>` clones in hot per-message paths. `Identity` is cheap to clone but the indirection compounds.

## Tests

- `cargo test -p vertex-swarm-<name>` per crate.
- `vertex-swarm-test-utils` provides `MockIdentity`, `MockTopology`, and cluster-shaped fixtures behind the `cluster` feature.
- `topology` and `node` rely on `libp2p-swarm-test` for behaviour tests; prefer that over hand-rolled mocks.
