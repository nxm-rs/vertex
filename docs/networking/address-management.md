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

### Self-Reachability Detection

`is_reachable()` (whether other public peers can reach us) returns true if any of:
- NAT addresses are configured (static public-scope addresses)
- Local listen addresses include public-scope IPs
- Reachability confirmed via inbound peer observation
- A verified external address (AutoNAT v2 dial-back or UPnP map)

Two signals can flip the self-reachability flag, in increasing order of confidence:

- `on_observed_addr()` is the weak signal, and it is fed only from **inbound** handshakes. When a peer dials us and the address it reports observing us at is public-scope, that is our genuinely reachable listen address (the peer reached it), so the flag is set. Observed addresses from outbound connections are deliberately ignored here: when we dial out, the remote sees our ephemeral NAT source port, which proves nothing about our reachability. The observed address itself is **not stored or advertised**, only the connectivity fact is recorded. Outbound-observed addresses still flow to the AutoNAT v2 client (via identify's `NewExternalAddrCandidate`) to be verified by dial-back.
- `on_external_addr_confirmed()` is the strong signal. It fires on `FromSwarm::ExternalAddrConfirmed`, which the libp2p swarm raises only after AutoNAT v2 has dialed one of our candidate addresses back, or UPnP has mapped a port. This is hard to spoof because it requires a completed inbound connection on the address under test. Unlike the observed signal it is reversible: `on_external_addr_expired()` drops the address on `FromSwarm::ExternalAddrExpired` (for example a UPnP lease that fails to renew), so a node whose only public path was a mapping that lapsed stops advertising itself as reachable.

### Peer reachability is not peer liveness

The per-peer `ReachabilityTracker` makes the same distinction for *other* nodes, and uses a vocabulary deliberately kept separate from address scope. Its verdict enum `PeerReachability` is `Unreachable`/`Unknown`/`Reachable` (ordered worst to best via `Ord`, so eviction drops the minimum first), never `Public`/`Private` - those words are reserved for `AddressScope`, the RFC IP-range classification. A peer can advertise a public-scope address yet be `Unreachable`, or sit on a private LAN address yet be `Reachable` to same-subnet peers.

`Reachable` means a peer accepts new inbound connections at its advertised address, which is only proven by an AutoNAT v2 dial-back (`on_autonat_peer_confirmed()`) or by us successfully dialing the peer outbound at a public-scope address (`on_outbound_reachable()`). A completed handshake or a successful ping is treated as **liveness** only: it clears the failure streak and recovers a demoted peer to `Unknown`, but never sets `Reachable`. This guards against the ephemeral-port trap: a NAT'd peer that dials us answers pings over its connection-specific inbound port yet is unreachable by anyone else, so it must not be ranked as reachable for eviction.

## AutoNAT v2 and UPnP

NAT traversal runs as a cluster of top-level libp2p behaviours that sit beside identify in each node type (`vertex-swarm-node`). They collaborate through the swarm external-address machinery rather than through direct calls.

- **identify** observes the remote-reported address and emits it as a `NewExternalAddrCandidate`.
- **AutoNAT v2 client** picks up each candidate, asks a peer that speaks `/libp2p/autonat/2/dial-request` to dial it back over a fresh port, and on success emits `ExternalAddrConfirmed`.
- **AutoNAT v2 server** performs dial-backs for other peers. A completed dial-back proves the remote peer accepts inbound connections, so the node promotes it to `Reachable` in the topology reachability tracker via `on_autonat_peer_confirmed()`.
- **UPnP** (`libp2p::upnp::tokio`) asks the LAN IGD gateway to map the listen port and emits `ExternalAddrConfirmed` for the mapped address.

The swarm broadcasts every `ExternalAddrConfirmed` to all behaviours, so the topology behaviour learns about verified addresses without any per-behaviour plumbing.

### Defaults and configuration

| Behaviour | Default | Flag |
|-----------|---------|------|
| AutoNAT v2 (client + server) | enabled for all node types | `--network.autonat` |
| UPnP port mapping | disabled (opt-in) | `--network.upnp` |

AutoNAT v2 runs both roles on every node type, including bootnodes, so the network always has dial-back verifiers. UPnP is opt-in because it actively probes the LAN gateway, which only helps home and NAT'd nodes and is noise on directly-routable hosts. Each behaviour is wrapped in a libp2p `Toggle`, so disabling one leaves an inert behaviour rather than changing the composed type.

### Interop

AutoNAT v2 (`/libp2p/autonat/2/dial-request`, `/libp2p/autonat/2/dial-back`) and UPnP are additive libp2p protocols. Peers that do not implement them simply never negotiate them, so the Swarm handshake and every `/swarm/...` protocol are unaffected. Until AutoNAT v2 support is widespread on the live network, a vertex node depends on other vertex nodes to act as its dial-back verifiers.

## Why Observed Addresses Are Not Stored

NAT-mapped addresses contain ephemeral ports that are connection-specific. Each inbound connection gets a different port assignment from the NAT, so the port reported by peer A only works for the A-to-us connection. If we advertise it to peer C via hive gossip, peer C cannot use it because the NAT mapping does not exist for C. Storing and advertising these addresses causes:

1. **Unbounded growth** - each new connection adds another ephemeral port variant
2. **Handshake encoding overflow** - too many multiaddrs exceed the 2048-byte protobuf buffer
3. **Network pollution** - hive gossip propagates unreachable addresses to all peers

### What Bee Does

Bee uses **static NAT configuration** (`--nat-addr`) for production deployments, not dynamic discovery. Bee also requires at least one underlay address in the handshake (empty underlays are rejected). The handshake protocol handles this by appending the peer-reported observed address as a last-resort fallback.

### IPv6 vs IPv4

Most IPv6 addresses are globally routable (except loopback, link-local, ULA, and documentation ranges). IPv4 is more complex due to NAT prevalence. For IPv4, only explicitly public listen addresses or configured NAT addresses are trusted.

