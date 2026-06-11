# Hive Gossip Strategy

This document describes the intelligent peer discovery gossip strategy used by the topology layer.

## Overview

Hive gossip distributes peer information based on network topology. The strategy optimises for:

1. **Neighbourhood replication**: neighbours need to know each other for chunk replication
2. **Bootstrap efficiency**: new peers need help finding their neighbourhood
3. **IPv4/IPv6 compatibility**: only gossip peers the recipient can actually reach

## Inbound records: verify on first dial

Every record received via hive passes full signature validation at the protocol layer (EIP-191 recovery, overlay recomputation, multiaddr peer-id presence) before topology ever sees it. Records that pass then go through a lightweight intake gate and straight into the peer manager as **unverified** entries:

- Unverified entries are fully dialable. Candidate selection treats them like any other supply; there is no separate verification dial, no warm-up phase, and no ephemeral prober identity. A successful real handshake both verifies and connects in one round trip.
- The handshake is the verification: the overlay, signature, and multiaddrs in the handshake come from the peer itself and overwrite the gossiped record. A handshake that asserts a different overlay than the dialed record demotes that record (an unverified claim is removed; a once-verified peer takes a dial failure).
- Failed dials use the peer manager's normal backoff. Unverified entries expire on a short failure budget (three consecutive failed dials marks them stale for the next purge), so junk gossip cannot pollute candidate supply.
- A record update for an already verified peer needs only the signature validation done at intake; it refreshes addresses without a dial and without clearing the verified bit.
- Unverified entries are never persisted in the peer snapshot and, because their node type is only provisional, never relayed onward by our own gossip selection (which picks handshake-confirmed storers).

### Intake gate

Two bounded mechanisms sit between validation and admission:

- **Per-overlay cooldown** (`record_cooldown`, default 5 minutes). Peers may re-sign their record on every broadcast, so the same overlay arrives repeatedly with a fresh signature and identical multiaddrs. Such records are dropped while the cooldown holds. A record whose multiaddrs changed bypasses the cooldown: it carries real news.
- **Per-gossiper budget** (`max_records_per_gossiper` admissions per cooldown window, default 64). One flooding source cannot fill the known table; the per-bin admission cap in the peer manager bounds it globally.

## Gossip Rules

### Neighbours (proximity >= depth)

Neighbours are critical for chunk replication. When a new neighbour joins:
- It receives all current neighbours (filtered by IP capability)
- All existing neighbours are notified about the new peer

### Distant Peers (proximity < depth)

Distant peers receive a targeted bootstrap set:

1. **Close peers**: peers near the recipient's overlay address (help find their neighbourhood)
2. **Diverse sample**: one peer from each bin for routing diversity

### Clients

Clients are invisible to gossip:
- Never gossiped about (they do not store chunks)
- Receive no peer lists (they connect to storers directly)

## Triggers

| Event | Action |
|-------|--------|
| Storer connects | Gossip based on neighbour/distant rules |
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
| `record_cooldown` | 5 minutes | Suppress re-signed records with unchanged multiaddrs |
| `max_records_per_gossiper` | 64 | Admissions per gossiper per cooldown window |
| `max_peers_for_distant` | 16 | Maximum peers sent to non-neighbours |
| `close_peers_count` | 4 | Close-to-recipient peers in bootstrap set |

## Implementation

The gossip coordinator is in `vertex-swarm-topology` (`gossip` module):

- `GossipTask` owns peer exchange, depth broadcasts, and record intake
- `GossipIntake` applies the cooldown and per-gossiper budget
- The peer manager (`vertex-swarm-peer-manager`) owns the unverified tier and the verify-on-handshake flip

## See Also

- [Protocols](protocols.md) - Hive protocol wire format
- [Peer Management](../networking/peer-management.md) - The unverified tier and stale policy
- [Client Architecture](../client/architecture.md) - PeerId/OverlayAddress boundary
