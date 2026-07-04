# Differences from Bee

This document tracks architectural and design differences between Vertex and the reference [Bee](https://github.com/ethersphere/bee) implementation of [Ethereum Swarm](https://ethswarm.org).

## Network Specification

### SwarmSpec Trait

Vertex introduces a `SwarmSpec` trait that defines network identity and protocol rules. This separates *what network* a node connects to from *how the node operates*.

**SwarmSpec provides:**
- Network identity (ID, name, underlying chain)
- Bootstrap nodes for peer discovery
- Hardfork activation schedule
- Token contract address

**SwarmSpec excludes (by design):**
- Storage capacity and policies
- Bandwidth pricing
- Cache strategies

This separation allows Clients and Storers to share the same spec while differing in operational parameters.

### Hardfork Support

Vertex has first-class support for protocol upgrades via hardforks:

- **`SwarmHardforks`**: manages fork activation conditions (timestamp-based)
- **`SwarmHardfork`**: enum of known protocol versions (e.g., `Accord`)
- **`ForkDigest`**: 4-byte keccak256-based identifier for verifying peer compatibility during handshake

Peers exchange `ForkDigest` values during connection to ensure they are running compatible protocol versions. The digest incorporates network ID, genesis timestamp, and active fork timestamps. Bee does not have explicit hardfork management infrastructure.

## Concrete Implementations

### Hive

`Hive` is the concrete implementation of `SwarmSpec` for mainnet, testnet, and development networks. Pre-configured specs are available via:

- `init_mainnet()`: production network on Gnosis Chain
- `init_testnet()`: test network on Sepolia
- `init_dev()`: local development with auto-generated network ID

Custom networks can be built with `HiveBuilder`.

## Postage Stamp Verification

The `nectar-postage` crate provides optimised postage stamp verification with several performance improvements over Bee.

### Cached Public Key Verification (~10x faster)

Bee performs ECDSA public key recovery for every stamp verification. nectar-postage allows caching the owner's public key after the first stamp in a batch, then using direct signature verification for subsequent stamps. This is particularly beneficial when validating many chunks from the same batch (common in retrieval and push-sync operations).

### Parallel Verification

The `parallel` feature enables rayon-based parallel verification across all CPU cores for batch stamp validation.

### Structural Validation Separation

nectar-postage separates structural validation (batch existence, expiry, index bounds, bucket matching) from cryptographic verification. This allows quick rejection of invalid stamps before expensive signature operations.

### no_std Support

Core types (`Stamp`, `Batch`, `StampIndex`, `StampDigest`) work without the standard library, enabling use in constrained environments. Storage and event handling require the `std` feature.

### k256 Precomputed Tables

nectar-postage enables k256's `precomputed-tables` feature for faster ECDSA operations, trading ~30KB binary size for improved verification speed.

### Architecture Comparison

| Component | Vertex (nectar-postage) | Bee |
|-----------|-------------------------|-----|
| `Stamp` | Immutable with verification methods | Tightly coupled to storage |
| `Batch` | Separate parameters with validation helpers | Integrated |
| `StampValidator` | Trait for custom strategies | Fixed implementation |
| `StoreValidator` | Combines `BatchStore` lookup with validation | Monolithic |
| `BatchStore` | Async trait for any backend | Internal storage only |
| `BatchEventHandler` | Separate blockchain event processing | Integrated |

Bee's postage implementation is tightly coupled to its internal storage. nectar-postage provides composable traits that can be implemented for different storage backends.

## Peer Scoring and Disconnect Attribution

Bee does not penalize a peer for disconnecting: its kademlia `Disconnected` handler removes the peer and sets a retry backoff, and blocklisting is driven only by accounting debt, never by connection timing. Vertex keeps a per-peer score and adds an early-disconnect signal, a deliberate divergence on the scoring side (the wire is unaffected).

The signal is deliberately narrow. Every locally-initiated close records its reason at the close site (`vertex_swarm_api::DisconnectReason`), so a close the node chose (bin trim, ban, low-score disconnect, bootnode rotation, shutdown) or an idle keep-alive teardown is never attributed to the peer. The penalty is reserved for a fast remote or transport close of a peer that did no useful work; a peer that served or accepted a chunk during the connection is exempt regardless of how it closed. This keeps honest serving peers, including mobile and browser clients whose transports reset under load, out of the penalty path while still scoring down a peer that handshakes and vanishes.

## Hive Gossip

Vertex speaks the hive wire protocol (`vertex-swarm-net-hive`, protocol id `/swarm/hive/2.0.0/peers`, thirty-record batches) byte-for-byte with the reference. Every divergence below is in gossip policy, who is told about which peers and when, and none of them changes a wire byte, so none needs a fork gate. The gossip engine lives in `vertex-swarm-topology`; the operator-facing behaviour and the tuning knobs are described in the hive gossip guide.

**Recipient-targeted bootstrap composition.** Gossiping its view to a distant peer, the reference sends a sample drawn at random per kademlia bin. Vertex composes the list for the specific recipient instead: the closest storers to that recipient, then one storer per bin for diversity, filling to sixteen. A joining node's first list therefore already points toward its own neighbourhood rather than a uniform cross-section. Wire-compatible: identical record type and batch framing, only the selection differs.

**Announcing a new storer to a subject-keyed sample.** When a distant storer connects, vertex announces it onward to a bounded per-bin sample of connected storers so third parties learn it without dialing, matching the reference's new-full-node broadcast in intent. The reference draws that sample at random; vertex chooses it deterministically but keyed on the announced peer, taking the storers closest to the newcomer by XOR distance within each bin. The selection therefore rotates with who is announced rather than favouring a fixed low-overlay set, so no storer is systematically excluded as a recipient, and the announcement flows toward the newcomer's own neighbourhood. Deterministic keeps the fan-out testable and needs no rng on wasm; the anti-amplification bound is the small per-bin cap either way. Wire-compatible: same record type and framing, only the recipient selection differs.

**Clients are gossip recipients but never subjects or sources.** Like the reference, vertex serves connecting light and browser clients discovery help: a client receives the recipient-targeted list on connect and is notified about each newly connected storer. This is parity of intent with the reference's light-node announce; only the composition is vertex's, as above. Client records are never placed in a payload and a client is never counted as a gossip source, so the population a client learns is always storers. Wire-compatible.

**Periodic neighbourhood refresh.** The reference re-gossips only in reaction to connection events. Vertex adds a periodic refresh (roughly ten minutes, `GossipConfig::refresh_interval`) that re-broadcasts the local neighbourhood view, so a quiet network keeps supply flowing without waiting for churn. It is bounded by the same per-recipient broadcast stamp that damps the event-driven path. Wire-compatible.

**Depth-decrease promotion.** When the locally observed neighbourhood depth decreases, meaning the neighbourhood widened, vertex gossips the newly promoted neighbours at once rather than waiting for the next connection event to surface them. The reference has no equivalent. This is a promptness gain over the same records. Wire-compatible.

**No outbound rate bucket.** The reference caps outbound gossip per peer. Vertex removed the outbound per-peer bucket: the broadcast cadence is already paced by the dial and refresh intervals, each payload is capped at the batch size, and an outbound bucket measurably starved legitimate gossip cycles after a few neighbourhood refreshes. Inbound throttling is unaffected. Wire-compatible.

**Inbound rate charged before signature recovery.** The reference rate-limits inbound gossip after the records are processed. Vertex charges its per-source rate bucket against the raw wire record count before any ECDSA recovery runs, so a flood of invalid signatures cannot bypass the throttle by being filtered out afterwards. This strengthens the reference posture rather than relaxing it. Wire-compatible.

## Bandwidth Accounting and Pseudosettle

Vertex prices chunks and tracks per-peer balances with the same arithmetic as Bee: the proximity price is `(max_po - proximity + 1) * base_price` with `base_price` 10000 and `max_po` 31, balances are signed (positive means the peer owes us), and the pseudosettle allowance is `min(requested, owed, refresh_rate * elapsed)`. Two behaviours on the pseudosettle path are deliberately not identical to Bee.

**First-contact allowance.** Bee seeds a peer's last-settlement time at the Unix epoch, so on the first pseudosettle the elapsed interval is effectively the whole epoch and the allowance is bounded only by the peer's debt: Bee accepts the full owed amount immediately. Vertex anchors the allowance clock at the moment it first accounts for a peer, so the first grant is bounded by the genuine wall-clock elapsed since first contact and ramps up over a few seconds at the configured refresh rate. This removes an unbounded first-contact grant (the only anti-free-ride brake on first contact and after a reconnect that cleared in-memory state) at the cost of a brief ramp before a fresh peer is granted its full allowance.

**Peer-advertised payment threshold.** Bee lets a peer advertise its own payment threshold and the debtor settles before crossing the advertised value. Vertex adopts the advertised threshold as a per-peer settle line, clamped to `[2x refresh rate, local payment threshold]`, and settles our debt to that peer against it; the value drives both the per-peer settle trigger the admission band reads and the settle fan-out early break. Two residual divergences remain. Bee disconnects a peer that advertises below its minimum, while vertex clamps the value up to `2x refresh rate` and keeps the peer (a local reaction policy, not a wire change). Bee also adopts an advertised threshold above its own default, while vertex caps adoption at the local payment threshold, so an announcement only ever tightens our settle timing and never widens the debt we let ourselves carry.

**Threshold growth across reconnects.** Both sides grow the announced serve line as a peer repays past checkpoints (linear `refresh rate * 100` steps to `refresh rate * 1800`, then doubling, one raise of one refresh rate per accepted repayment, re-announced on the pricing stream). Bee resets the announced line and checkpoint to the node-type default on every connect but keeps a process-lifetime repayment accumulator and persists its receiver-side settlement totals, so after a reconnect the line re-climbs one checkpoint per repayment against the lifetime total. Vertex resets the accumulator and the checkpoint together with the line on every connect: growth is earned within a connection and starts afresh after a reconnect. This only slows how fast a reconnecting peer regains credit; the schedule and the wire messages are identical.

**No creditor disconnect margin.** When a checkpoint raises the line Bee also recalculates a creditor-side disconnect limit at `(100 + tolerance)%` of the new line and blocklists a peer whose debt crosses it. Vertex has no creditor blocklist line: enforcement is refusal at the serve line itself, which moves with each raise, so the raised credit is enforced by the provide gate rather than a disconnect margin. A misbehaving debtor is refused service instead of disconnected, a local reaction policy with no wire footprint.

**Refreshment debt cap.** Bee caps an accepted inbound refreshment at the peer's debt including its shadow reserve (in-flight, uncommitted provides). Vertex caps at the committed positive balance only: the shadow reserve is a racy projection, and crediting repayment against provides that can still be released would book payment for service never rendered. The stricter cap can only defer acceptance (the ack tells the peer what was accepted, and the next refreshment covers the remainder once the provides commit), never over-credit. Wire-compatible either way.

## Summary of Key Differences

| Area | Vertex | Bee |
|------|--------|-----|
| Network Spec | `SwarmSpec` trait with hardfork support | Hardcoded configuration |
| Hardforks | First-class `SwarmHardfork` enum, `ForkDigest` | No explicit support |
| Postage Verification | Cached pubkeys, parallel, structural separation | Per-stamp recovery |
| Modularity | Composable traits, pluggable backends | Monolithic implementation |
| no_std Support | Core types work without std | Requires std |
| Hive Gossip | Recipient-targeted composition, client recipients, periodic and depth-decrease broadcasts, inbound rate charged before crypto, no outbound bucket | Random per-bin sample, event-driven only, post-hoc inbound limit, outbound bucket |
| Disconnect Scoring | Activity-gated early-disconnect penalty, intent-attributed closes | No disconnect penalty |
| Pseudosettle First Contact | Allowance ramps from first-contact clock | Full debt accepted immediately |
| Payment Threshold | Adopted, clamped to [2x refresh rate, local payment threshold]; announced per connection and re-announced at repayment checkpoints; growth resets with the connection; refusal at the serve line, no creditor disconnect margin | Honours peer-advertised threshold, advertises and grows its own from a lifetime repayment total; blocklists at `(100 + tolerance)%` of the line |
| Refreshment Debt Cap | Committed positive balance only | Debt including shadow reserve |

## See Also

- [Architecture Overview](../architecture/overview.md) - High-level design
- [Hive Gossip](hive-gossip.md) - Gossip rules, recipient composition, and client policy
- [Swarm API](api.md) - Protocol trait definitions
