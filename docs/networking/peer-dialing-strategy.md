# Peer Dialing Strategy

Design notes for peer discovery, bootstrapping, and connection retry logic.

## Goals

1. **Fast bootstrapping** - Connect to the network quickly on startup
2. **Resilient retry** - Don't give up on peers that temporarily reject us
3. **Efficient resource use** - Don't waste bandwidth on peers unlikely to accept
4. **Bin coverage** - Maintain Kademlia bin targets for proper routing

## Why Peers Reject Connections

A peer disconnecting or refusing a connection doesn't mean they're bad:

| Reason | Action |
|--------|--------|
| Peer is full (connection limit) | Retry later with backoff |
| Transient network issue | Retry later with backoff |
| Peer is restarting | Retry later with backoff |
| Protocol mismatch | Mark as incompatible, deprioritize |
| Peer banned us | Stop retrying (if detectable) |
| Peer offline permanently | Eventually prune after many failures |

**Key insight**: Most rejections are temporary. Aggressive deletion loses valuable peer knowledge.

## Connection States

```
                    ┌─────────────────────────────────────┐
                    │                                     │
                    ▼                                     │
┌─────────┐    ┌───────────┐    ┌───────────┐    ┌──────────────┐
│  Known  │───▶│Connecting │───▶│ Connected │───▶│ Disconnected │
└─────────┘    └───────────┘    └───────────┘    └──────────────┘
     ▲              │                                     │
     │              │ (dial failed)                       │
     │              ▼                                     │
     │         ┌─────────┐                                │
     └─────────│ Failed  │◀───────────────────────────────┘
               └─────────┘          (connection lost)
                    │
                    │ (after backoff expires)
                    ▼
               Retry as Known
```

## Dial Tracking Fields

Add to `PeerState`:

```rust
struct DialTracking {
    /// When we last attempted to dial this peer
    last_dial_attempt: Option<Instant>,

    /// Consecutive dial failures (reset on success)
    consecutive_failures: u32,

    /// Total lifetime dial attempts
    total_dial_attempts: u64,

    /// Total lifetime successful connections
    total_connections: u64,

    /// Last successful connection time
    last_connected: Option<Instant>,
}
```

## Exponential Backoff

After a failed dial, wait before retrying:

```
base_delay = 30 seconds
max_delay = 1 hour
delay = min(base_delay * 2^consecutive_failures, max_delay)
```

| Failures | Delay |
|----------|-------|
| 0 | 0 (first attempt) |
| 1 | 30s |
| 2 | 1m |
| 3 | 2m |
| 4 | 4m |
| 5 | 8m |
| 6 | 16m |
| 7+ | 1h (capped) |

**Jitter**: Add random jitter (0-25% of delay) to prevent thundering herd.

## Dial Candidate Selection

When Kademlia needs connections for a bin, select candidates using:

```rust
fn select_dial_candidates(bin: &Bin, max_candidates: usize) -> Vec<OverlayAddress> {
    bin.known_peers()
        .filter(|p| is_dialable(p))
        .filter(|p| backoff_expired(p))
        .sorted_by(dial_priority)
        .take(max_candidates)
        .collect()
}

fn dial_priority(peer: &PeerState) -> impl Ord {
    // Priority order (higher = dial sooner):
    // 1. Never attempted (newest discoveries first for freshness)
    // 2. Previously connected (proven to work)
    // 3. Fewer consecutive failures
    // 4. Longer since last attempt (LRU)

    (
        peer.total_connections > 0,           // Previously connected
        -(peer.consecutive_failures as i32),  // Fewer failures
        peer.last_dial_attempt,               // LRU (None = highest priority)
    )
}

fn backoff_expired(peer: &PeerState) -> bool {
    match peer.last_dial_attempt {
        None => true,  // Never attempted
        Some(last) => {
            let delay = backoff_delay(peer.consecutive_failures);
            last.elapsed() >= delay
        }
    }
}
```

## Bootstrapping Strategy

### Phase 1: Bootnode Connection

1. Dial bootnodes in parallel (configured list)
2. Stop after reaching `min_bootnode_connections` (default: 3)
3. Don't wait for slow bootnodes - move on after first success

### Phase 2: Hive Discovery

1. Connected peers send us their known peers via Hive protocol
2. Store dialable peers in PeerManager with multiaddrs
3. Add stored overlays to Kademlia (only those we can actually dial)
4. Kademlia evaluates which bins need filling

### Phase 3: Bin Filling

1. Kademlia identifies bins below target (e.g., < 4 connected)
2. For each underfilled bin, select dial candidates (respecting backoff)
3. Dial candidates, update tracking on success/failure
4. Signal dial_notify when new candidates are ready

### Continuous Maintenance

After initial bootstrap:

1. **Periodic evaluation** - Kademlia's manage loop checks bin health
2. **Event-driven dialing** - New hive peers trigger immediate evaluation
3. **Backoff expiry** - Peers become dialable again after backoff
4. **Connection churn** - Disconnections trigger replacement searches

## Pruning Strategy

Don't delete peers aggressively. Prune only when:

```rust
fn should_prune(peer: &PeerState) -> bool {
    // Never prune recently active peers
    if peer.last_connected.map(|t| t.elapsed() < Duration::hours(24)).unwrap_or(false) {
        return false;
    }

    // Prune if: many failures AND never connected AND old
    peer.consecutive_failures >= 10
        && peer.total_connections == 0
        && peer.created_at.elapsed() > Duration::days(7)
}
```

**Rationale**: A peer we connected to yesterday might be temporarily offline. A peer we discovered a week ago and never successfully connected to is likely invalid.

## Implementation Checklist

- [ ] Add `DialTracking` fields to `PeerState`
- [ ] Implement `backoff_expired()` check in `filter_dialable_candidates()`
- [ ] Update `start_connecting()` to set `last_dial_attempt`
- [ ] Update `on_connected()` to reset `consecutive_failures`, set `last_connected`
- [ ] Update `connection_failed()` to increment `consecutive_failures`
- [ ] Add jitter to backoff calculation
- [ ] Implement pruning in periodic maintenance task
- [ ] Add metrics for dial success rate, backoff distribution

## Metrics to Track

| Metric | Purpose |
|--------|---------|
| `peer_dial_attempts_total` | Total dial attempts (label: result) |
| `peer_dial_backoff_seconds` | Histogram of backoff durations |
| `peer_consecutive_failures` | Histogram of failure counts |
| `kademlia_bin_fill_ratio` | Connected/target per bin |
| `peer_store_size` | Total known peers |
| `peer_dialable_count` | Peers eligible for dialing now |

## Open Questions

1. **Bin prioritization**: Should we prioritize filling closer bins (higher PO) over distant ones?
2. **Parallel dial limit**: How many concurrent dials per bin? Global limit?
3. **Success ratio threshold**: Should peers with <10% success rate be deprioritized further?
4. **Network partition detection**: How do we detect we're isolated vs peers are unavailable?
