# Hive Gossip Strategy

This document describes the intelligent peer discovery gossip strategy used by the topology layer.

## Overview

Hive gossip distributes peer information based on network topology. The strategy optimises for:

1. **Neighbourhood replication**: neighbours need to know each other for chunk replication
2. **Bootstrap efficiency**: new peers need help finding their neighbourhood
3. **IPv4/IPv6 compatibility**: only gossip peers the recipient can actually reach

## Gossip Rules

### Neighbours (proximity >= depth)

Neighbours are critical for chunk replication. When a new neighbour joins:
- It receives all current neighbours (filtered by IP capability)
- All existing neighbours are notified about the new peer

### Distant Peers (proximity < depth)

Distant peers receive a targeted bootstrap set:

1. **Close peers**: peers near the recipient's overlay address (help find their neighbourhood)
2. **Diverse sample**: one peer from each bin for routing diversity

### Light Nodes

Light nodes are invisible to gossip:
- Never gossiped about (they do not store chunks)
- Receive no peer lists (they connect to storers directly)

## Triggers

| Event | Action |
|-------|--------|
| Full node connects | Gossip based on neighbour/distant rules |
| Depth decreases | Notify newly-promoted neighbours |
| Periodic tick | Refresh stale neighbourhood peers |

## IP Capability Filtering

Before gossiping, peers are filtered by IP version compatibility:

| Recipient | Can receive |
|-----------|-------------|
| IPv4-only | IPv4-only or dual-stack peers |
| IPv6-only | IPv6-only or dual-stack peers |
| Dual-stack | All peers |

This prevents gossiping unreachable addresses.

## Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `refresh_interval` | 10 minutes | How often to refresh neighbours |
| `max_peers_for_distant` | 16 | Maximum peers sent to non-neighbours |
| `close_peers_count` | 4 | Close-to-recipient peers in bootstrap set |

## Implementation

The gossip manager is in `vertex-swarm-topology`:

- `HiveGossipManager` tracks broadcast times and depth changes
- `TopologyBehaviour::enable_gossip()` enables gossip with a depth provider
- Gossip triggers after successful ping/pong (proves bidirectional connectivity)

## See Also

- [Protocols](protocols.md) - Hive protocol wire format
- [Client Architecture](../client/architecture.md) - PeerId/OverlayAddress boundary
