# Address Management

Protocol-agnostic network utilities for libp2p-based P2P networks, provided by `vertex-net-peer`.

## Address Classification

Classifies IP addresses into scopes for smart address selection:

| Scope | IPv4 | IPv6 |
|-------|------|------|
| Loopback | 127.0.0.0/8 | ::1 |
| Private | 10/8, 172.16/12, 192.168/16 | fd00::/8 (ULA) |
| Link-local | 169.254.0.0/16 | fe80::/10 |
| Public | Everything else | Everything else |

The `IpCapability` type tracks dual-stack support (IPv4, IPv6, or both) and is used to filter peer addresses we can actually dial.

## Local Network Detection

Queries system network interfaces to determine subnet membership. This enables accurate "same subnet" checks without hardcoding subnet sizes.

Uses the `netdev` crate supporting Linux, macOS, Windows, Android, iOS, and BSDs.

Interface information is cached for 60 seconds to avoid repeated system calls.

### Special Cases

- **Loopback** (127.x.x.x, ::1): Always considered same network with other loopback
- **Link-local** (169.254.x.x, fe80::/10): Always considered same network with other link-local
- **Different IP versions**: Never on the same subnet
- **Unspecified** (0.0.0.0, ::): Never on any network

## AddressManager

Manages multiaddr selection for handshake and peer advertisement.

### Address Types

| Type | Description |
|------|-------------|
| Listen | Addresses from libp2p we're listening on |
| NAT | Configured external addresses for NAT traversal |
| Observed | Addresses peers report seeing us at (auto-NAT) |

### Selection Logic

When selecting addresses for a peer based on their connection scope:

| Peer Scope | Addresses Returned |
|------------|-------------------|
| Loopback | Loopback + private listen addresses |
| Private | Same-subnet private + NAT addresses |
| Public | Public listen + NAT + confirmed observed |

### Observed Address Confirmation

To prevent a single malicious peer from claiming false addresses:

1. Observed addresses require confirmation from 2+ unique IPs (same protocol family)
2. Only public peers can confirm public observed addresses
3. Confirmed addresses are cached with 1-hour TTL
4. Pending observations are capped at 10 entries (LRU eviction)
5. Confirmed cache is capped at 20 entries (LRU eviction)

### Security Considerations

- Private peers cannot confirm public addresses (prevents address spoofing)
- Confirmations must come from the same IP family (IPv4 peer confirms IPv4 addresses)
- Duplicate confirmations from the same IP are ignored
