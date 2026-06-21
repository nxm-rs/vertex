# Design Proposal: Composite Behaviour Layering for Client and Storer Node Types

## Summary

Today `vertex-swarm-topology` is a standalone composite `NetworkBehaviour` (handshake + hive + ping), but the client protocols (pricing, retrieval, pushsync, pseudosettle, swap) live hand-rolled inside `vertex-swarm-node` and the storer has no protocols of its own (`StorerNode` wraps `ClientNode` with stubs). This note plans two new libp2p-aware composite crates that mirror `topology`, so the behaviour stack matches the node-type hierarchy: `topology` is wrapped by a client composite, which is wrapped by a storer composite.

This supersedes the "aspirational, not current" paragraph in `crates/swarm/AGENTS.md`: the composite crates are now the plan of record. The load-bearing constraint carries over unchanged: the hand-rolled `ClientBehaviour`/`ClientHandler` multiplexer is moved as-is, never reshaped into derived sub-behaviours.

## Target layering

Each tier is the one below it plus the protocols that tier introduces. Membership is a strict subset along the node-type hierarchy.

```
                          ┌─────────────────────────────────────────────┐
  storer composite        │ vertex-swarm-storer-behaviour                │
  (StorerNode)            │   #[derive(NetworkBehaviour)]                 │
                          │   client:   ClientBehaviour (the tier below) │
                          │   pullsync: PullsyncBehaviour  ◄── new        │
                          └───────────────────────┬─────────────────────┘
                                                  │ contains
                          ┌───────────────────────▼─────────────────────┐
  client composite        │ vertex-swarm-client-behaviour                │
  (ClientNode)            │   one hand-rolled NetworkBehaviour over       │
                          │   headered substreams:                        │
                          │     pricing, pseudosettle, swap,              │
                          │     retrieval, pushsync                       │
                          └───────────────────────┬─────────────────────┘
                                                  │ wraps (composed at node level)
                          ┌───────────────────────▼─────────────────────┐
  topology composite      │ vertex-swarm-topology  (exists)              │
  (BootNode and up)       │   #[derive(NetworkBehaviour)]                 │
                          │   handshake, hive, ping                       │
                          └─────────────────────────────────────────────┘
```

Protocol-to-tier map:

- topology: `/swarm/handshake`, `/swarm/hive`, `/ipfs/ping`.
- client: `/swarm/pricing`, `/swarm/pseudosettle`, `/swarm/swap` (feature-gated), `/swarm/retrieval`, `/swarm/pushsync`.
- storer: `/swarm/pullsync` (cursors stream + range-sync stream).

Note the asymmetry. The client tier is a single hand-rolled `NetworkBehaviour` that internally multiplexes five protocols (it does not `#[derive(NetworkBehaviour)]` over five sub-behaviours, and must not be made to). The topology and storer tiers are derived composites of distinct sub-behaviours. The node-type derived behaviour (`ClientNodeBehaviour`) keeps the infra sub-behaviours (`connection_limits`, `identify`, `nat`, `topology`) alongside the client composite; see the open question on whether those move down.

## E1: Extract the client composite crate

Create `vertex-swarm-client-behaviour` (libp2p-aware tier, `crates/swarm/`). This is a pure move plus a re-export shim, no behaviour change.

Move out of `crates/swarm/node/src/protocol/`:

- `behaviour.rs`: `ClientBehaviour`, its `Config`/`BehaviourConfig`.
- `handler.rs`: `ClientHandler`, `Config` (including the split `retrieval_timeout`/`pushsync_timeout`/`timeout` fields), `HandlerCommand`, `HandlerEvent`.
- `upgrade.rs`: `ClientInboundUpgrade` and the per-protocol dispatch over `UpgradeInfo`.
- `forward.rs`: the `Forwarder` trait, `StubForwarder`, `NetworkForwarder`, the strictly-closer loop rule, and the deferred-credit accounting seam.
- `storer.rs`: `StorerCapability` (reserve + receipt-signing identity; implements `ReceiptSigner`).

The crate depends down on:

- `vertex-swarm-client-protocol` (the `ClientCommand`/`ClientEvent`/`ChunkTransferError`/`RetrievalResult`/`PseudosettleEvent`/`SwapEvent` contract added to break the settlement-to-node cycle).
- the `vertex-swarm-net-*` codec-and-upgrade crates: `pricing`, `pseudosettle`, `pushsync`, `retrieval`, `swap`, `headers`.
- `vertex-swarm-api` traits (`SwarmLocalStore`, `ReserveStore`, the accounting and topology traits the `NetworkForwarder` consumes).
- `vertex-swarm-primitives`.

It does NOT depend on `vertex-swarm-node`, on the concrete accounting crates, or on the node builders. The `NetworkForwarder` already takes its topology, accounting, and reporter as `api`-trait objects, so nothing in the moved code names a concrete implementation; this is why the move is mechanical.

Stays in `vertex-swarm-node`:

- `PeerSelector` (`selection.rs`): score- and affordability-aware candidate ordering. Builder-side, not in the behaviour.
- `SelfThrottle` (`throttle.rs`): per-peer outbound pacing under the pseudosettle allowance. Builder-side.
- `ClientHandle` and `ClientService` (`client_service.rs`): the command/event bridge and the outbound request API. Builder-side.
- `ClientNode`/`ClientNodeBuilder`, `StorerNode`/`StorerNodeBuilder`, `BootNode`, the `ClientNodeBehaviour` derived composite, `BaseNode`, the launch path, and all node lifecycle.

`vertex-swarm-node` re-exports every moved item from its original paths (the `protocol::` re-export surface in `lib.rs` and `protocol/mod.rs`), so downstream code and the workspace see no path change. E1 is the keystone: nothing else lands until the extraction is green, and it is the one step where a careless move could silently alter the multiplexer's substream, back-pressure, or timeout behaviour. Prove code-equivalence by a filtered diff of the moved files against their origin, not by reasoning about a rewrite.

## E2: PullsyncBehaviour

Add a `PullsyncBehaviour` (its home is decided in E3; likely the storer composite crate). It is one `NetworkBehaviour` wiring the `vertex-swarm-net-pullsync` drivers, which already exist on the `feat/pullsync` branch over two streams:

- Inbound (syncer): answer a peer's `Syn` with an `Ack` of per-bin cursors (`CursorsResponder`), then serve a `Get` range as `Offer` -> `Want` -> `Delivery` (`SyncResponder`), reading from a `PullStorage` (the `BinCursorStore` reserve snapshot plus `reserve_epoch`). Enforces `MAX_CHUNKS_PER_SECOND` and `PAGE_TIMEOUT` at the behaviour layer.
- Outbound (puller): a command/event surface that drives `CursorsOutboundProtocol` then `SyncOutboundProtocol` (`SyncRequester`) against a chosen neighbour, emitting delivered chunks for admission.

The puller service proper (readiness gating, interval persistence, the verifier seam) is E4; E2 lands only the wire-driving behaviour and its inbound serve path so it can be composed.

## E3: The storer composite crate

Create `vertex-swarm-storer-behaviour` (libp2p-aware tier, `crates/swarm/`):

```rust
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "StorerBehaviourEvent")]
pub struct StorerBehaviour {
    client:   ClientBehaviour,     // the E1 composite
    pullsync: PullsyncBehaviour,   // E2
}
```

`StorerNode` then gets its own derived node behaviour (parallel to `ClientNodeBehaviour`) composing the infra sub-behaviours with `StorerBehaviour`, instead of today's `StorerNode { client: ClientNode }` wrapper with `TODO` stubs. The storer's `StorerCapability` (already moved in E1) installs on the contained `ClientBehaviour` exactly as it does now via `enable_storage`; pullsync adds the neighbour-driven fill path that the pushsync ingest path does not cover.

## Dependency direction and no-cycle argument

The boundary that the `vertex-swarm-client-protocol` crate established (settlement crates depend down on the contract, not up on `node`) is the same boundary the composites preserve:

```
  net-* codecs ──┐
                 ├──► client-behaviour ──► storer-behaviour ──► node ──► builder
  client-protocol┘         │                     │
  api traits ──────────────┴─────────────────────┘
```

- `client-behaviour` depends only down: on `client-protocol`, the `net-*` codecs, `api` traits, and `primitives`. Never on `node`, never on concrete accounting.
- `storer-behaviour` depends on `client-behaviour`, `net-pullsync`, and `api` traits. Never on `node`.
- `node` depends up the chain on both composites and selects one per node type.
- The concrete accounting crates (`accounting/pseudosettle`, `accounting/swap`) continue to depend on `client-protocol` for their event types, not on either composite or on `node`.

No edge points back up, so no cycle. The `NetworkForwarder` keeping its dependencies as `api` trait objects is what lets `client-behaviour` sit below `node` without re-introducing the cycle.

## Composing with the merged storer-builder surface

The storer-builder stack already merged provides the seams the composite consumes; this note adds no new builder surface, it wires into the existing one.

- Served reserve: `StorerComponents<T, C, S, R>` carries the cache-then-reserve serve view (`S`, `HasStore`) and the proximity-ordered reserve (`R`, `HasReserve` over `BinCursorStore`). The `PullsyncBehaviour` inbound syncer reads its `PullStorage` snapshot from that same `R`, so cursors and range answers come from the one reserve the components already name.
- gRPC: the native `ReserveService<R>` (over `BinCursorStore`) already serves `GetReserveState` and `GetReserveBins` for operator visibility; the puller and syncer observe the same reserve, so the RPC view and the sync view never diverge.
- Serve store: the builder's `CacheThenReserve` (reserve-first reads, cache-only writes) is the retrieval-serve view the contained `ClientBehaviour` already uses on a storer; pullsync writes admitted chunks into the reserve through its own handle, not through this serve view, matching how pushsync ingest is wired today.
- Chain: the builder already requires a chain for storer (and swap-enabled) builds, so the puller's verifier (E4) can assume on-chain batch access is present on a storer.

## Sequenced PR plan

| Step | Scope | Depends on | Parallelism |
|---|---|---|---|
| E1 | `vertex-swarm-client-behaviour` extraction: pure move of `ClientBehaviour`/`ClientHandler`/`upgrade`/`forward`/`StorerCapability`, with `node` re-exporting from the original paths. No behaviour change. | - | serial (keystone) |
| E2 | `PullsyncBehaviour`: wire-driving behaviour over the `net-pullsync` drivers, inbound serve from `PullStorage`, outbound command/event surface. | `feat/pullsync` merged | parallel with E1 |
| E3 | `vertex-swarm-storer-behaviour`: derived composite of `client` + `pullsync`; `StorerNode` gets its own behaviour. | E1, E2 | serial after E1 and E2 |
| E4 | Puller service: readiness-gated (the topology `wait_until_neighbourhood_ready` surface), interval persistence (`IntervalStore`), `PullChunkVerifier` admission seam (signature-only interim impl is valid). | E2 | parallel with E3 |
| E5 | node/builder/RPC integration: select the storer composite per node type, wire the puller and syncer to `StorerComponents`/`HasReserve` and `ReserveService`, and reserve furthest-eviction on admission. | E3, E4 | serial last |

E1 is the load-bearing step: it relocates the multiplexer that must keep its substream, back-pressure, and per-protocol-timeout behaviour byte-for-byte. E2 and E4 touch only new pullsync code and can proceed alongside the E1/E3 spine.

## Risks and open questions

- `PeerSelector`/`SelfThrottle` accounting coupling. Both consume `SwarmClientAccounting`/`PeerAffordability` from `api`, but they live builder-side in `node`, not in the composite. Confirm that keeping them in `node` (rather than in `client-behaviour`) is right: the behaviour stays accounting-agnostic and the builder injects pacing, which is the current shape. If a future puller wants the same affordability signal for neighbour selection, decide whether that seam is shared or storer-local.
- Infra sub-behaviours placement. `connection_limits`, `identify`, and `nat` currently sit in the node-type derived `ClientNodeBehaviour`, not in topology or the client composite. Decide whether they stay node-level (clean per-node-type assembly, but each node type re-lists them) or move into the client composite (DRY across client and storer, but couples the composite to NAT/identify). Leaning node-level to keep the composites protocol-pure.
- `PullsyncBehaviour` crate home. It may live in the storer composite crate or in its own crate. Prefer the storer composite crate unless a non-storer consumer needs it, keeping `crates/swarm/net/` one-protocol-per-crate.
- Receipt and ingest overlap. A storer takes custody via two paths now: pushsync ingest (`StorerCapability` on `ClientBehaviour`) and, after this work, pullsync fill. Confirm the admission and eviction rules (`evict_furthest`, batch eviction) stay consistent across both so the reserve does not double-count or diverge.
