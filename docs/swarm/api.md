# Swarm API Architecture

The `swarm-api` crate defines the core protocol traits for Swarm operations. These traits define *what* Swarm does, not *how* it's implemented.

## Node Types

Vertex has three node types:

| Node Type | Capabilities | Traits Implemented |
|-----------|--------------|-------------------|
| **Bootnode** | Topology only (peer discovery) | `BootnodeTypes` |
| **Client** | Topology + retrieval + upload | `BootnodeTypes`, `ClientTypes` |
| **Storer** | Full capabilities including local storage | Full trait hierarchy |

## Core Traits

### BootnodeTypes

Base trait for all nodes. Provides network participation.

```rust
pub trait BootnodeTypes: Clone + Send + Sync {
    type Spec: SwarmSpec + Clone;
    type Identity: Identity<Spec = Self::Spec>;
    type Topology: Topology + Clone;
    type Node: SpawnableTask;
    type ClientService: SpawnableTask;
    type ClientHandle: Clone + Send + Sync + 'static;
}
```

### ClientTypes

Extends BootnodeTypes with bandwidth accounting for data transfer.

```rust
pub trait ClientTypes: BootnodeTypes {
    type Accounting: BandwidthAccounting;
}
```

### StorerTypes

Full storage node capabilities with local storage and synchronization.

```rust
pub trait StorerTypes: ClientTypes {
    type Store: LocalStore + Clone;
    type Sync: ChunkSync + Clone;
}
```

## Identity Trait

The core `Identity` trait defines node identity:

```rust
pub trait Identity: Clone + Send + Sync + 'static {
    type Spec: SwarmSpec + Clone;
    type Signer: Signer + SignerSync + Clone;

    fn spec(&self) -> &Self::Spec;
    fn nonce(&self) -> B256;
    fn signer(&self) -> Arc<Self::Signer>;
    fn node_type(&self) -> SwarmNodeType;
    fn overlay_address(&self) -> SwarmAddress;
    fn ethereum_address(&self) -> Address;
    fn is_full_node(&self) -> bool;
    fn welcome_message(&self) -> Option<&str>;
}
```

## Component Containers

Each node type has a corresponding component container:

- `SwarmBaseComponents` - Base components (identity + topology)
- `SwarmClientComponents` - Client nodes (base + accounting)
- `SwarmStorerComponents` - Storer nodes (client + store + sync)

## Key Traits

| Trait | Purpose | Node Types |
|-------|---------|------------|
| `Identity` | Node identity and signing | All |
| `Topology` | Peer discovery and routing | All |
| `BandwidthAccounting` | Per-peer bandwidth tracking | Client, Storer |
| `LocalStore` | Local chunk persistence | Storer only |
| `ChunkSync` | Chunk synchronization | Storer only |

## Protocol Integration

- `SwarmProtocol` - Implements `vertex_node_api::Protocol`
- `SwarmServices` - Unified services for all node types

## Design Principles

### 1. Traits Define What, Implementations Define How
The API defines behavior contracts without specifying implementation details.

### 2. No libp2p Leakage
All operations use `OverlayAddress` (32-byte Swarm address), not libp2p `PeerId`. The mapping happens in the client layer.

### 3. Lock-Free Bandwidth Accounting
Per-peer handles use atomics for `record()` operations. Multiple protocols can record concurrently without contention.

## Bandwidth Accounting Design

Two-level design to avoid lock contention:

1. `BandwidthAccounting` - Factory that creates per-peer handles
2. `PeerBandwidth` - Per-peer handle with lock-free operations

Accounting uses overlay addresses (not `PeerId`) because:
- Accounting is tied to Swarm identity, not connection
- A peer may reconnect with different multiaddr but same overlay
- Settlement (SWAP cheques) is based on overlay identity

## Default Accounting Values

| Parameter | Default Value |
|-----------|---------------|
| Mode | Pseudosettle |
| Base price | 10,000 AU per chunk |
| Refresh rate | 4,500,000 AU/second |
| Payment threshold | 13,500,000 AU |
| Tolerance | 25% |
| Early payment | 50% |
| Light factor | 10 |

## See Also

- [Node Types](../architecture/node-types.md) - Bootnode, Client, Storer details
- [Client Architecture](../client/architecture.md) - libp2p integration
- [Protocols](protocols.md) - Network protocol patterns
