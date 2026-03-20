# Address Management

## Overview

Address management in Vertex spans two crates:

- **`vertex-net-local`** - Protocol-agnostic address classification, capability tracking, and subnet detection
- **`vertex-swarm-topology` (`LocalAddressManager`)** - Swarm-specific address selection for handshake advertisement

## Address Classification (`vertex-net-local`)

Classifies IP addresses into scopes for smart address selection:

| Scope | IPv4 | IPv6 |
|-------|------|------|
| Loopback | 127.0.0.0/8 | ::1 |
| Private | 10/8, 172.16/12, 192.168/16 | fd00::/8 (ULA) |
| Link-local | 169.254.0.0/16 | fe80::/10 |
| Public | Everything else | Everything else |

`IpCapability` tracks dual-stack support (IPv4, IPv6, or both) and filters peer addresses we can actually dial.

## Local Network Detection

`LocalCapabilities` queries system network interfaces to determine subnet membership. This enables accurate "same subnet" checks without hardcoding subnet sizes.

Uses the `netdev` crate supporting Linux, macOS, Windows, Android, iOS, and BSDs. Interface information is cached for 60 seconds.

### Special Cases

- **Loopback** (127.x.x.x, ::1): Always considered same network with other loopback
- **Link-local** (169.254.x.x, fe80::/10): Always considered same network with other link-local
- **Different IP versions**: Never on the same subnet
- **Unspecified** (0.0.0.0, ::): Never on any network

## LocalAddressManager (Topology)

Manages address selection for the Swarm handshake protocol. Located in `vertex-swarm-topology::nat_discovery`.

### Address Sources

| Source | Priority | Description |
|--------|----------|-------------|
| NAT | Highest | Static addresses configured via `--nat-addr` |
| Listen | Normal | Addresses from libp2p we're listening on |

### Scope-Based Selection

When selecting addresses for a peer during handshake:

| Peer Scope | Addresses Returned |
|------------|-------------------|
| Loopback | Loopback listen addresses only (no NAT) |
| Private | Same-subnet private + NAT addresses |
| Public | Public listen + NAT addresses |

All returned addresses include `/p2p/{local_peer_id}`.

### Public Connectivity Detection

`has_public_addresses()` returns true if any of:
- NAT addresses are configured (static public addresses)
- Local listen addresses include public IPs
- Confirmed public connectivity via peer observation

When a peer reports our address via identify, `on_observed_addr()` sets a boolean public connectivity flag if the observed address is public. This flag enables dialing other public peers. The observed address itself is **not stored or advertised** - only the connectivity fact is recorded.

## Why Observed Addresses Are Not Stored

NAT-mapped addresses contain ephemeral ports that are connection-specific. Each inbound connection gets a different port assignment from the NAT, so the port reported by peer A only works for the A-to-us connection. If we advertise it to peer C via hive gossip, peer C cannot use it because the NAT mapping does not exist for C. Storing and advertising these addresses causes:

1. **Unbounded growth** - each new connection adds another ephemeral port variant
2. **Handshake encoding overflow** - too many multiaddrs exceed the 2048-byte protobuf buffer
3. **Network pollution** - hive gossip propagates unreachable addresses to all peers

### What Bee Does

Bee uses **static NAT configuration** (`--nat-addr`) for production deployments, not dynamic discovery. Bee also requires at least one underlay address in the handshake (empty underlays are rejected). The handshake protocol handles this by appending the peer-reported observed address as a last-resort fallback.

### IPv6 vs IPv4

Most IPv6 addresses are globally routable (except loopback, link-local, ULA, and documentation ranges). IPv4 is more complex due to NAT prevalence. For IPv4, only explicitly public listen addresses or configured NAT addresses are trusted.

