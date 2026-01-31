# Node Types

Vertex supports three node types, each with different capabilities and resource requirements.

## Node Types

| Node Type | Description | Use Case |
|-----------|-------------|----------|
| **Bootnode** | Topology only (handshake, hive, pingpong). No pricing or accounting. | Network infrastructure, peer discovery |
| **Client** | Read + write (retrieval, pushsync, pricing, bandwidth accounting). Consumes the network without storing chunks locally. | Content access, uploads, dApp backends |
| **Storer** | Storage + staking (pullsync, local storage, redistribution). Stores chunks locally and participates in the storage incentive game. | Network infrastructure, earning rewards |

## CLI Selection

```bash
vertex node --mode <bootnode|client|storer>  # default: client
```

## Service Requirements

| Node Type | Hive | Bandwidth | Retrieval | Upload | Pullsync | Redistribution |
|-----------|:----:|:---------:|:---------:|:------:|:--------:|:--------------:|
| **Bootnode** | Yes | No | No | No | No | No |
| **Client** | Yes | Yes | Yes | Yes | No | No |
| **Storer** | Yes | Yes | Yes | Yes | Yes | Optional |

## Service Descriptions

### Hive/Topology
Kademlia peer discovery and routing table maintenance. All nodes participate in the DHT to find peers and route requests.

### Bandwidth Accounting
Fair resource usage tracking via Pseudosettle (soft accounting) and/or SWAP (payment channels with chequebook). Required for any data transfer.

### Retrieval
Fetching chunks from peers using the retrieval protocol. Requires bandwidth accounting.

### Upload (Pushsync)
Writing chunks to the network. Requires valid postage stamps.

### Pullsync
Synchronizing chunks with neighborhood peers. Storage nodes pull chunks they're responsible for based on overlay address proximity.

### Redistribution
Participating in the storage incentive game. Requires staking BZZ tokens. Nodes prove they store chunks and earn rewards.

## Identity Requirements

| Node Type | Wallet | Nonce | Reason |
|-----------|:------:|:-----:|--------|
| **Bootnode** | Ephemeral OK | Ephemeral OK | No incentives, no storage responsibility |
| **Client** | Persistent recommended | Ephemeral OK | Postage batches tied to wallet |
| **Storer** | Persistent | Persistent | Overlay address determines storage responsibility, staking tied to wallet |

## Bandwidth Accounting Modes

Bandwidth accounting can be configured independently of node type (except Bootnode which has no accounting):

| Mode | Description | Use Case |
|------|-------------|----------|
| **None** | No accounting | Bootnodes only (automatically set) |
| **Pseudosettle** | Soft accounting without real payments | Default for Client/Storer |
| **SWAP** | Payment channels with chequebook | Production with real payments |
| **Both** | Pseudosettle until threshold, then SWAP | Hybrid approach |

## Protocol Dependency Diagram

```
                    ┌─────────────┐
                    │   Storer    │
                    │             │
                    │ pullsync    │
                    │ localstore  │
                    │ redistrib.  │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │   Client    │
                    │             │
                    │ retrieval   │
                    │ pushsync    │
                    │ bandwidth   │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │  Bootnode   │
                    │             │
                    │ hive        │
                    │ kademlia    │
                    │ pingpong    │
                    └─────────────┘
```

Each layer builds on the one below. A Storer node runs all protocols from Bootnode through Pullsync/Redistribution.

## Implementation Status

| Node Type | Status |
|-----------|--------|
| **Bootnode** | Implemented |
| **Client** | Implemented |
| **Storer** | Not yet implemented |

## Type System Representation

The node types are represented internally as capability traits:

```
BootnodeTypes (base - topology only)
       │
       ▼
ClientTypes (adds BandwidthAccounting)
       │
       ▼
StorerTypes (adds LocalStore + ChunkSync)
```

Each node type implements the corresponding trait hierarchy.

## See Also

- [CLI Configuration](../cli/configuration.md) - How to configure each node type
- [Swarm API](../swarm/api.md) - Protocol trait definitions
