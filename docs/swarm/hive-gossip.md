# Hive Gossip Strategy

This document describes the intelligent peer discovery gossip strategy used by the topology layer.

## Overview

Hive gossip distributes peer information based on network topology. The strategy optimizes for:

1. **Neighborhood replication** - Neighbors need to know each other for chunk replication
2. **Bootstrap efficiency** - New peers need help finding their neighborhood
3. **IPv4/IPv6 compatibility** - Only gossip peers the recipient can actually reach

## Gossip Rules

### Neighbors (proximity >= depth)

Neighbors are critical for chunk replication. They receive **all other neighborhood peers**:

```
New neighbor joins
       │
       ├──> Send: all current neighbors (filtered by IP capability)
       │
       └──> Notify: all existing neighbors about new peer
```

### Distant Peers (proximity < depth)

Distant peers receive a targeted bootstrap set:

1. **Close peers** - Peers near the recipient's overlay address (help find their neighborhood)
2. **Diverse sample** - One peer from each bin for routing diversity

### Light Nodes

Light nodes are invisible to gossip:
- Never gossiped about (they don't store chunks)
- Receive no peer lists (they connect to storers directly)

## Triggers

| Event | Action |
|-------|--------|
| Full node connects | Gossip based on neighbor/distant rules |
| Depth decreases | Notify newly-promoted neighbors |
| Periodic tick | Refresh stale neighborhood peers |

## IP Capability Filtering

Before gossiping, peers are filtered by IP version compatibility:

| Recipient | Can receive |
|-----------|-------------|
| IPv4-only | IPv4-only or dual-stack peers |
| IPv6-only | IPv6-only or dual-stack peers |
| Dual-stack | All peers |

This prevents gossiping unreachable addresses.

## Configuration

```rust
HiveGossipConfig {
    refresh_interval: Duration,    // How often to refresh neighbors (default: 10min)
    max_peers_for_distant: usize,  // Max peers for non-neighbors (default: 16)
    close_peers_count: usize,      // Close-to-recipient peers (default: 4)
}
```

## Implementation

The gossip manager is in `vertex-swarm-topology`:

- `HiveGossipManager` - Tracks broadcast times and depth changes
- `TopologyBehaviour::enable_gossip()` - Enables gossip with a depth provider
- Gossip triggers after successful ping/pong (proves bidirectional connectivity)

## See Also

- [Protocols](protocols.md) - Hive protocol wire format
- [Client Architecture](../client/architecture.md) - PeerId/OverlayAddress boundary
