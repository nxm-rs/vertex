# CLI Configuration

This document describes the CLI argument organization and how they map to node configuration.

## Node Mode Selection

```bash
--mode <MODE>               Node mode: bootnode, client, storer
                            [default: client]
```

The node mode determines which services are enabled and what CLI arguments are relevant. See [Node Types](../architecture/node-types.md) for details.

## CLI Argument Groups

Arguments are organized into logical groups that correspond to node subsystems.

### Network (`--network.*`)

P2P networking configuration.

```
--network.port <PORT>           P2P listen port [default: 1634]
--network.addr <ADDR>           P2P listen address [default: 0.0.0.0]
--network.bootnodes <ADDRS>     Bootstrap node multiaddresses (comma-separated)
--network.trusted-peers <ADDRS> Trusted peers to connect to (comma-separated)
--network.max-peers <N>         Maximum peer connections [default: 50]
--network.idle-timeout <SECS>   Idle connection timeout [default: 30]
--network.no-discovery          Disable peer discovery
--network.nat-addr <ADDRS>      External addresses to advertise (comma-separated)
--network.nat-auto              Auto-NAT discovery from peers
```

### Bandwidth (`--bandwidth.*`)

Bandwidth accounting configuration. Applies to Client and Storer nodes (Bootnode has no accounting).

```
--bandwidth.mode <MODE>         Accounting mode: none, pseudosettle, swap, both
                                [default: pseudosettle]
--bandwidth.threshold <AU>      Payment threshold before settlement
                                [default: 13,500,000 AU]
--bandwidth.tolerance-percent <0-100>
                                Disconnect tolerance as percentage of threshold
                                [default: 25]
--bandwidth.base-price <AU>     Price per chunk in Accounting Units
                                [default: 10,000 AU]
--bandwidth.refresh-rate <AU/s> Pseudosettle refresh rate
                                [default: 4,500,000 AU/second]
--bandwidth.early-percent <0-100>
                                Early settlement trigger percentage
                                [default: 50]
--bandwidth.light-factor <NUM>  Light node scaling factor
                                [default: 10]
```

### Storage (`--storage.*`)

Local storage configuration. Relevant for Storer nodes.

```
--storage.chunks <NUM>          Storage capacity in chunks [default: 2^22]
--cache.chunks <NUM>            Cache capacity in chunks [default: 2^16]
--redistribution                Participate in redistribution game (storer only)
```

### Identity

Node identity and keystore configuration.

```
--password <PWD>                Keystore password (env: VERTEX_PASSWORD)
--password-file <PATH>          Read password from file
--ephemeral                     Use random ephemeral identity (not recommended for Storer)
--nonce <HEX>                   Overlay address nonce (32-byte hex string)
```

### Network Selection

```
--mainnet                       Connect to mainnet (Gnosis Chain)
--testnet                       Connect to testnet (Sepolia)
--swarmspec <PATH>              Custom SwarmSpec configuration file
```

## Configuration Resolution

CLI arguments are merged with the SwarmSpec (network specification) to produce the final node configuration:

```
┌─────────────┐     ┌─────────────┐     ┌─────────────────┐
│  CLI Args   │────>│   Merge     │────>│  NodeConfig     │
└─────────────┘     │             │     │                 │
                    │             │     │ - mode          │
┌─────────────┐     │             │     │ - network       │
│  SwarmSpec  │────>│             │     │ - bandwidth     │
│ (mainnet/   │     └─────────────┘     │ - storage       │
│  testnet)   │                         │ - identity      │
└─────────────┘                         └─────────────────┘
```

**SwarmSpec provides:**
- Network ID
- Bootnodes (can be overridden by CLI)
- Contract addresses (postage, staking, chequebook factory)
- Default pricing parameters

**CLI provides:**
- Node mode selection
- Local configuration (ports, paths, capacity)
- Feature toggles (which accounting mode)
- Overrides for network defaults

## Common Configurations

### Bootnode

Network infrastructure for peer discovery only:

```bash
vertex node --mainnet --mode=bootnode
```

### Client (Default)

Content retrieval and upload:

```bash
vertex node --mainnet --mode=client --bandwidth.mode=pseudosettle
```

Or simply (uses defaults):

```bash
vertex node --mainnet
```

### Storer (Not Yet Implemented)

Storage node with redistribution:

```bash
vertex node --mainnet \
  --mode=storer \
  --storage.chunks=4194304 \
  --redistribution \
  --password-file=/etc/vertex/password
```

## Bandwidth Modes

| Mode | Description | Use Case |
|------|-------------|----------|
| **none** | No accounting | Bootnodes (set automatically) |
| **pseudosettle** | Soft accounting without real payments | Default, testing, trusted networks |
| **swap** | Payment channels with chequebook | Production with real payments |
| **both** | Pseudosettle until threshold, then SWAP | Hybrid approach |

## Implementation Status

| Feature | Status |
|---------|--------|
| Network configuration | Implemented |
| Bandwidth accounting (pseudosettle) | Implemented |
| Identity/keystore | Implemented |
| Bootnode mode | Implemented |
| Client mode | Implemented |
| Storer mode | Not yet implemented |
| SWAP payments | Not yet implemented |
| Redistribution | Not yet implemented |

## See Also

- [Node Types](../architecture/node-types.md) - Detailed node type descriptions
- [Swarm API](../swarm/api.md) - Protocol traits and accounting
