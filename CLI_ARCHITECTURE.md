# CLI Architecture

This document describes the CLI argument organization and how they map to node configuration.

## Node Types

Swarm nodes operate as one of five types, each building on the capabilities of the previous:

| Node Type | Description | Use Case |
|-----------|-------------|----------|
| **Bootnode** | Only participates in topology (Kademlia/Hive) | Network infrastructure, peer discovery |
| **Light** | Can retrieve chunks from the network | Read-only clients, browsers, mobile apps |
| **Publisher** | Can retrieve + upload chunks | Content publishers, dApp backends |
| **Full** | Stores chunks for the network | Network infrastructure, altruistic storage |
| **Staker** | Full + redistribution game participation | Earning rewards for storage |

### Service Requirements by Node Type

Each node type requires specific services to be enabled:

| Node Type | Hive | Bandwidth | Retrieval | Upload | Pullsync | Redistribution |
|-----------|:----:|:---------:|:---------:|:------:|:--------:|:--------------:|
| **Bootnode** | Yes | - | - | - | - | - |
| **Light** | Yes | Yes | Yes | - | - | - |
| **Publisher** | Yes | Yes | Yes | Yes | - | - |
| **Full** | Yes | Yes | Yes | Yes | Yes | - |
| **Staker** | Yes | Yes | Yes | Yes | Yes | Yes |

### Service Descriptions

1. **Hive/Topology** - Kademlia peer discovery and routing table maintenance. All nodes participate in the DHT to find peers and route requests.

2. **Bandwidth Accounting** - Fair resource usage tracking via Pseudosettle (soft accounting) and/or SWAP (payment channels with chequebook).

3. **Retrieval** - Fetching chunks from peers using the retrieval protocol. Requires bandwidth accounting to pay for data transfer.

4. **Upload/Postage** - Writing chunks to the network. Requires valid postage stamps (batches) to pay for storage. Includes batch management and stamp attachment.

5. **Pullsync** - Synchronizing chunks with neighborhood peers. Storage nodes pull chunks they're responsible for based on their overlay address proximity.

6. **Redistribution** - Participating in the storage incentive game. Requires staking BZZ tokens. Nodes prove they store chunks and earn rewards.

### Identity Requirements

| Node Type | Wallet | Nonce | Reason |
|-----------|:------:|:-----:|--------|
| **Bootnode** | Ephemeral | Ephemeral | No incentives, no storage responsibility |
| **Light** | Ephemeral OK | Ephemeral OK | SWAP optional (pseudosettle works without) |
| **Publisher** | Persistent | Ephemeral OK | Postage batches tied to wallet |
| **Full** | Persistent | Persistent | Overlay address determines storage responsibility |
| **Staker** | Persistent | Persistent | Staking contract tied to wallet, overlay determines neighborhood |

## Incentive Layers

### Bandwidth Incentives

Bandwidth incentives ensure fair usage of network resources for chunk retrieval.

| Mode | Description | Use Case |
|------|-------------|----------|
| **None** | No accounting | Bootnodes, dev/testing only |
| **Pseudosettle** | Soft accounting without real payments | Light clients, trusted networks |
| **SWAP** | Payment channels with chequebook | Production networks |

Both Pseudosettle and SWAP can be enabled simultaneously - SWAP takes over when
the pseudosettle threshold is reached.

### Storage Incentives

| Component | Who Pays | Who Earns | Description |
|-----------|----------|-----------|-------------|
| **Postage** | Publishers | - | Pay to upload chunks (buy postage batches) |
| **Redistribution** | - | Stakers | Earn rewards for storing chunks in your neighborhood |

## CLI Argument Groups

Arguments are organized into logical groups that correspond to node subsystems.

### Node Type (`--type`)

```
--type <TYPE>               Node type: bootnode, light, publisher, full, staker
                            [default: light]
```

The node type determines which services are enabled and what CLI arguments are relevant.

### Network (`--network.*`)

P2P networking configuration - how the node connects to the Swarm.

```
--network.port <PORT>           P2P listen port [default: 1634]
--network.addr <ADDR>           P2P listen address [default: 0.0.0.0]
--network.bootnodes <ADDRS>     Bootstrap node multiaddresses (comma-separated)
--network.max-peers <N>         Maximum peer connections [default: 50]
--network.no-discovery          Disable peer discovery
```

### Topology (`--topology.*`)

Kademlia topology and peer selection settings.

```
--topology.neighborhood <DEPTH> Target neighborhood depth [default: auto]
--topology.reserve <N>          Reserve capacity for neighborhood peers
```

### Bandwidth (`--bandwidth.*`)

Bandwidth incentive configuration for retrieval. Required for Light, Publisher, Full, and Staker nodes.

```
--bandwidth.mode <MODE>         Incentive mode: none, pseudosettle, swap, or both
                                [default: pseudosettle]
--bandwidth.threshold <BYTES>   Payment threshold before settlement [default: 10GB]
--bandwidth.disconnect <BYTES>  Disconnect threshold for unpaid debt [default: 100GB]
--bandwidth.price <PLUR>        Price per chunk in PLUR [default: from swarmspec]
```

When `--bandwidth.mode=both`, pseudosettle is used until the threshold is reached,
then SWAP cheques are issued.

### SWAP (`--swap.*`)

SWAP payment channel configuration. Required when `--bandwidth.mode` includes swap.

```
--swap.endpoint <URL>           Ethereum RPC endpoint for SWAP transactions
--swap.chequebook <ADDR>        Chequebook contract address (auto-deployed if not set)
--swap.initial-deposit <BZZ>    Initial chequebook deposit [default: 10 BZZ]
```

### Storage (`--storage.*`)

Local chunk storage configuration. Required for Full and Staker nodes.

```
--storage.capacity <GB>         Maximum storage capacity [default: 10]
--storage.path <PATH>           Storage database path [default: <datadir>/localstore]
--storage.cache-size <MB>       In-memory cache size [default: 1024]
--storage.reserve-capacity <N>  Reserve capacity for neighborhood chunks [default: 4194304]
```

### Postage (`--postage.*`)

Postage stamp configuration for uploading chunks. Required for Publisher, Full, and Staker nodes.

```
--postage.endpoint <URL>        Ethereum RPC endpoint for postage transactions
--postage.batch <ID>            Default postage batch ID for uploads
```

### Redistribution (`--redistribution.*`)

Storage incentive configuration for earning rewards. Required for Staker nodes.

```
--redistribution.endpoint <URL> Ethereum RPC endpoint for redistribution transactions
--redistribution.stake <BZZ>    Stake amount for redistribution [default: 10 BZZ]
```

### Identity (`--identity.*` or top-level)

Node identity and keystore configuration.

```
--password <PWD>                Keystore password (env: VERTEX_PASSWORD)
--password-file <PATH>          Read password from file
--ephemeral                     Use random ephemeral identity (not valid for Full/Staker)
```

### API (`--api.*`)

HTTP/gRPC API configuration.

```
--api.http                      Enable HTTP API
--api.http-addr <ADDR>          HTTP listen address [default: 127.0.0.1]
--api.http-port <PORT>          HTTP listen port [default: 1633]
--metrics                       Enable metrics endpoint
--metrics.addr <ADDR>           Metrics listen address [default: 127.0.0.1]
--metrics.port <PORT>           Metrics listen port [default: 1637]
```

## Configuration Resolution

CLI arguments are merged with the SwarmSpec (network specification) to produce
the final node configuration:

```
┌─────────────┐     ┌─────────────┐     ┌─────────────────┐
│  CLI Args   │────▶│   Merge     │────▶│  NodeConfig     │
└─────────────┘     │             │     │                 │
                    │             │     │ - node_type     │
┌─────────────┐     │             │     │ - network       │
│  SwarmSpec  │────▶│             │     │ - topology      │
│ (mainnet/   │     └─────────────┘     │ - bandwidth     │
│  testnet)   │                         │ - storage       │
└─────────────┘                         │ - api           │
                                        └─────────────────┘
```

SwarmSpec provides:
- Network ID
- Bootnodes (can be overridden by CLI)
- Contract addresses (postage, staking, chequebook factory, etc.)
- Default pricing parameters

CLI provides:
- Node type selection
- Local configuration (ports, paths, capacity)
- Feature toggles (which incentives to enable)
- Overrides for network defaults

## Common Configurations

### Bootnode

Network infrastructure for peer discovery only:

```bash
vertex node --mainnet --type=bootnode
```

### Light Reader (Pseudosettle)

Minimal configuration for reading from the network:

```bash
vertex node --mainnet --type=light --bandwidth.mode=pseudosettle
```

### Light Reader (SWAP)

Light client with real payments:

```bash
vertex node --mainnet \
  --type=light \
  --bandwidth.mode=swap \
  --swap.endpoint=https://rpc.gnosis.io
```

### Publisher Node

Upload content with postage stamps:

```bash
vertex node --mainnet \
  --type=publisher \
  --postage.endpoint=https://rpc.gnosis.io \
  --postage.batch=<batch-id> \
  --api.http
```

### Full Storage Node

Store chunks for the network (altruistic, no rewards):

```bash
vertex node --mainnet \
  --type=full \
  --storage.capacity=1000 \
  --swap.endpoint=https://rpc.gnosis.io \
  --password-file=/etc/vertex/password
```

### Staker Node

Full storage with redistribution rewards:

```bash
vertex node --mainnet \
  --type=staker \
  --storage.capacity=1000 \
  --redistribution.stake=10 \
  --swap.endpoint=https://rpc.gnosis.io \
  --redistribution.endpoint=https://rpc.gnosis.io \
  --password-file=/etc/vertex/password
```

## Implementation Status

| Group | Status | Notes |
|-------|--------|-------|
| Network | Implemented | Basic P2P working |
| Topology | Partial | Hive protocol working, depth config TODO |
| Bandwidth | TODO | Need pseudosettle and SWAP implementations |
| SWAP | TODO | Requires chequebook contract integration |
| Storage | TODO | Need persistent chunk store |
| Postage | TODO | Requires postage contract integration |
| Redistribution | TODO | Requires staking contract integration |
| Identity | Implemented | Keystore and password handling |
| API | Partial | Flags exist, server not implemented |

## Protocol Dependencies

```
                    ┌─────────────┐
                    │   Staker    │
                    │             │
                    │ redistrib.  │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │    Full     │
                    │             │
                    │  pullsync   │
                    │  localstore │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │  Publisher  │
                    │             │
                    │   postage   │
                    │   pusher    │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │    Light    │
                    │             │
                    │  retrieval  │
                    │  bandwidth  │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │  Bootnode   │
                    │             │
                    │    hive     │
                    │  kademlia   │
                    └─────────────┘
```

Each layer builds on the one below it. A Staker node runs all protocols from Bootnode through Redistribution.
