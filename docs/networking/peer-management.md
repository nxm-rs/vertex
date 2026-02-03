# Peer Management

Protocol-agnostic peer state management with Arc-per-peer pattern, provided by `vertex-net-peers`.

## Architecture

```
┌───────────────────────────────────────────────────────┐
│                  NetPeerManager<Id>                   │
│  ┌────────────────┐  ┌────────────────┐              │
│  │ peers: RwLock  │  │ PeerRegistry   │              │
│  │ <HashMap<Id,   │  │ Id ↔ PeerId    │              │
│  │  Arc<State>>>  │  │ bidirectional  │              │
│  └────────────────┘  └────────────────┘              │
└───────────────────────────────────────────────────────┘
                              │
                    Arc::clone() (cheap)
                              │
           ┌──────────────────┼──────────────────┐
           ▼                  ▼                  ▼
   RetrievalHandler    PushSyncHandler    PricingHandler
   (holds Arc clone)   (holds Arc clone)  (holds Arc clone)
                              │
                              ▼
              ┌───────────────────────────────┐
              │       PeerState<Id>           │
              │  ┌─────────────────────────┐  │
              │  │ Atomics (lock-free)     │  │
              │  │ - score                 │  │
              │  │ - state                 │  │
              │  │ - latency               │  │
              │  │ - counters              │  │
              │  └─────────────────────────┘  │
              │  ┌─────────────────────────┐  │
              │  │ Per-peer RwLock (cold)  │  │
              │  │ - multiaddrs            │  │
              │  │ - ban_info              │  │
              │  └─────────────────────────┘  │
              └───────────────────────────────┘
```

## Arc-per-Peer Pattern

The core design principle: protocol handlers get `Arc<PeerState>` once, then all subsequent operations are lock-free (atomics) or per-peer locked (no global contention).

### Lock Contention Analysis

| Operation | Lock | Contention |
|-----------|------|------------|
| Get peer Arc | Global map read | Brief, amortized by caching Arc |
| Create new peer | Global map write | Rare (once per peer lifetime) |
| Score update | None (atomic) | Zero |
| State check | None (atomic) | Zero |
| Latency update | None (atomic) | Zero |
| Multiaddr update | Per-peer RwLock | Zero with other peers |
| Connected peers list | Global map read | Brief iteration |

## Core Types

### NetPeerId (Blanket Trait)

Any type implementing `Clone + Eq + Hash + Send + Sync + Debug + Serialize + Deserialize` automatically implements `NetPeerId`. No explicit implementation needed.

```rust
#[derive(Clone, Hash, Eq, PartialEq, Debug, Serialize, Deserialize)]
struct OverlayAddress([u8; 32]);

// Works automatically!
let manager = NetPeerManager::<OverlayAddress>::with_defaults();
```

### PeerState<Id>

Per-peer state with atomic hot paths and per-peer locked cold paths.

**Atomic fields (hot path):**
- `score` - Fixed-point reputation score
- `state` - ConnectionState as u8
- `latency_nanos` - Last measured RTT
- `connection_successes`, `connection_timeouts`, `protocol_errors` - Counters
- `last_seen` - Unix timestamp
- `is_full_node` - Node capability flag

**Locked fields (cold path):**
- `multiaddrs` - Known addresses for this peer
- `ban_info` - Ban metadata if banned

### ConnectionState

```rust
enum ConnectionState {
    Known,       // Discovered but not connected
    Connecting,  // Dial in progress
    Connected,   // Handshake complete
    Disconnected,// Was connected, may reconnect
    Banned,      // Will not reconnect
}
```

### PeerRegistry<Id>

Bidirectional mapping between protocol IDs (e.g., `OverlayAddress`) and libp2p `PeerId`.

Handles:
- Peer reconnection with different PeerId (returns `RegisterResult::Replaced`)
- Same peer, same PeerId reconnection (returns `RegisterResult::SamePeer`)
- Peer changing overlay address (old mapping removed)

### EventEmitter<Id>

Non-blocking broadcast channel for peer events.

Events:
- `Discovered` - New peer added to manager
- `Connecting` - Dial started
- `Connected` - Handshake complete
- `Disconnected` - Connection closed
- `Banned` / `Unbanned` - Ban status changed
- `StateChanged` - Any state transition
- `ScoreBelowThreshold` - Score dropped below ban threshold

Slow subscribers drop events independently (no backpressure on other subscribers).

## Persistence

### NetPeerStore Trait

```rust
trait NetPeerStore<Id>: Send + Sync {
    fn load_all(&self) -> Result<Vec<NetPeerSnapshot<Id>>, PeerStoreError>;
    fn save(&self, snapshot: &NetPeerSnapshot<Id>) -> Result<(), PeerStoreError>;
    fn save_batch(&self, snapshots: &[NetPeerSnapshot<Id>]) -> Result<(), PeerStoreError>;
    fn remove(&self, id: &Id) -> Result<(), PeerStoreError>;
    fn get(&self, id: &Id) -> Result<Option<NetPeerSnapshot<Id>>, PeerStoreError>;
    fn count(&self) -> Result<usize, PeerStoreError>;
    fn clear(&self) -> Result<(), PeerStoreError>;
    fn flush(&self) -> Result<(), PeerStoreError>;
}
```

Auto-impl provided for `&T`, `Box<T>`, `Arc<T>`.

### Implementations

| Store | Use Case |
|-------|----------|
| `MemoryPeerStore` | Testing, no persistence |
| `FilePeerStore` | JSON file, atomic writes via temp file + rename |

## Usage

```rust
use vertex_net_peers::{NetPeerManager, PeerEvent};

let manager = NetPeerManager::<OverlayAddress>::with_defaults();

// Get peer state (cached by protocol handlers)
let peer = manager.peer(overlay);

// Atomic operations (hot path)
peer.record_success(latency);
peer.add_score(1.0);
let score = peer.score();

// Per-peer locked operations (cold path)
peer.update_multiaddrs(addrs);

// Connection lifecycle
manager.start_connecting(id);
manager.on_connected(id, peer_id, is_full_node);
manager.on_disconnected_by_peer_id(&peer_id);

// Event subscription
let mut rx = manager.subscribe();
while let Ok(event) = rx.recv().await {
    match event {
        PeerEvent::Connected { id, .. } => { /* ... */ }
        PeerEvent::Banned { id, reason } => { /* ... */ }
        _ => {}
    }
}

// Persistence
manager.load_from_store(&store)?;
manager.save_to_store(&store)?;
```

## Scoring

Score is maintained atomically using fixed-point arithmetic (scaled by 100,000). Clamped to [-1,000,000, +1,000,000].

Default score adjustments:
- `record_success()`: +1.0
- `record_timeout()`: -1.5
- `record_protocol_error()`: -3.0

Custom adjustments via `add_score(delta)` or `set_score(value)`.

## Thread Safety

All types are `Send + Sync`. The design ensures:

1. **Global map lock** held briefly to get `Arc<PeerState>`
2. **No global lock** needed after obtaining the Arc
3. **Per-peer RwLock** only contends with same-peer operations
4. **Atomics** for all hot-path operations (score, state, counters)

Protocol handlers should cache the `Arc<PeerState>` to avoid repeated map lookups.

## Relationship to vertex-net-peer

| Crate | Scope |
|-------|-------|
| `vertex-net-peer` | Single-peer utilities: address classification, NAT, local network detection |
| `vertex-net-peers` | Multi-peer management: registry, state, events, persistence |

Both are protocol-agnostic and operate below the Swarm layer.
