# Architecture Overview

This document describes the high-level architecture of Vertex and how its crates are organized.

## Design Principles

1. **Modularity** - Every component is designed to be used as a library: well-tested, documented, and benchmarked. Developers can import individual components and build custom solutions.

2. **Layered Abstraction** - Clear separation between protocol definitions (traits) and implementations. The `swarm-api` crate defines *what* Swarm does; implementation crates define *how*.

3. **libp2p Boundary** - libp2p dependencies are confined to specific crates (`client-core`, `net-*`). Higher-level crates like `swarm-api` and `swarm-core` are libp2p-agnostic, enabling alternative transports and easier testing.

4. **Type Safety** - Use of Rust's type system to enforce correctness. The `NodeTypes` trait pattern ensures components are compatible at compile time.

## Crate Organization

```
vertex/crates/
├── primitives/           # Core types (PeerId wrapper, OverlayAddress)
│
├── swarm/                # Swarm-specific abstractions (10 crates)
│   ├── spec/             # Network specification (ID, hardforks, bootnodes)
│   ├── forks/            # Hardfork definitions (timestamp-based)
│   ├── api/              # Swarm PROTOCOL traits (libp2p-free)
│   ├── primitives/       # Swarm-specific types (SwarmNodeType, overlay)
│   ├── identity/         # SwarmIdentity implementation
│   ├── peer/             # Peer types (libp2p boundary for Multiaddr)
│   ├── core/             # CLI config, re-exports from client-core
│   ├── builder/          # Swarm node builder
│   ├── node/             # Swarm-specific CLI entry points
│   └── rpc/              # gRPC services (ChunkService, NodeService)
│
├── client/               # Client layer (libp2p boundary)
│   ├── core/             # THE BOUNDARY - SwarmNode, NodeBehaviour
│   ├── bandwidth/        # Bandwidth accounting
│   │   ├── core/         # Accounting and pricing
│   │   ├── chequebook/   # Chequebook types
│   │   ├── pseudosettle/ # Pseudosettle provider
│   │   └── swap/         # SWAP provider
│   └── topology/         # Network topology
│       ├── core/         # Topology behaviour (libp2p)
│       ├── kademlia/     # Kademlia implementation
│       └── peermanager/  # Peer management
│
├── net/                  # Network primitives and protocols
│   ├── codec/            # Protocol codec abstractions
│   └── protocols/        # Protocol implementations (all libp2p)
│       ├── handshake/    # Peer authentication (SYN/SYNACK/ACK)
│       ├── headers/      # Distributed tracing + negotiation
│       ├── hive/         # Peer discovery and gossip
│       ├── pingpong/     # Connection liveness (RTT)
│       ├── pricing/      # Payment threshold announcements
│       ├── pseudosettle/ # Bandwidth accounting payments
│       ├── pushsync/     # Chunk push/receipt
│       ├── retrieval/    # Chunk request/response
│       └── swap/         # SWAP settlement protocol
│
├── node/                 # Generic node infrastructure (libp2p-free)
│   ├── types/            # NodeTypes trait (DatabaseProvider, RpcServer)
│   ├── api/              # Protocol trait, NodeContext
│   ├── builder/          # Generic node building framework
│   ├── commands/         # CLI commands infrastructure
│   └── core/             # Node infrastructure (logging, tracing)
│
├── rpc/
│   ├── core/             # gRPC service traits
│   └── server/           # gRPC server implementation
│
├── storage/              # Storage abstraction layer
├── storer/
│   └── core/             # Storer node implementation
│
├── metrics/              # Observability (Prometheus, tracing)
└── tasks/                # Task executor abstraction
```

### libp2p Dependency Summary

**Crates WITH libp2p (15 total):**
- `client/core` (THE BOUNDARY)
- `client/topology/*` (3 crates)
- `net/protocols/*` (9 protocol crates)
- `primitives` (for PeerId)
- `swarm/peer` (for Multiaddr)

**Crates WITHOUT libp2p (25+ crates):**
- All `swarm/*` except `peer`
- All `node/*` (generic infrastructure)
- All `rpc/*`
- `client/bandwidth/*` (4 crates)
- `storage`, `storer`, `metrics`, `tasks`

## Key Abstractions

### NodeTypes Trait Pattern

Following the reth pattern, we define stateless traits that carry associated types:

```rust
// Generic node infrastructure (node/types)
pub trait NodeTypes: Clone + Debug + Send + Sync + 'static {
    type Database: DatabaseProvider;
    type Rpc: RpcServer;
    type Executor: TaskExecutor;
}

// Swarm capability traits (swarm/api)
pub trait BootnodeTypes: Clone + Send + Sync {
    type Spec: SwarmSpec;
    type Identity: Identity;
    type Topology: Topology;
    // ...
}

pub trait ClientTypes: BootnodeTypes {
    type Accounting: BandwidthAccounting;
}

pub trait StorerTypes: ClientTypes {
    type Store: LocalStore;
    type Sync: ChunkSync;
}
```

This enables generic node construction while maintaining type safety.

### Protocol vs Node Architecture

| Layer | Purpose | Examples |
|-------|---------|----------|
| **Protocol (swarm-api)** | Defines *what* Swarm does | `Identity`, `Topology`, `BandwidthAccounting`, `LocalStore` |
| **Node (node-api)** | Defines *how* a node is composed | `NodeTypes`, `Protocol`, `NodeContext` |

### Dependency Flow

```
                    ┌─────────────┐
                    │ primitives  │
                    └──────┬──────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
        ┌─────▼─────┐ ┌────▼────┐ ┌─────▼─────┐
        │ swarmspec │ │swarm-api│ │   net/*   │
        └─────┬─────┘ └────┬────┘ └─────┬─────┘
              │            │            │
              └────────────┼────────────┘
                           │
                    ┌──────▼──────┐
                    │ node/types  │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │  node/api   │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │node/builder │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │  node/core  │
                    └─────────────┘
```

## Component Builder Pattern

Each subsystem has a dedicated builder trait for composition:

```rust
pub trait ChunkStoreBuilder<N: NodeTypes>: Send {
    type ChunkStore: ChunkStore<ChunkSet = N::ChunkSet>;
    async fn build(self, ctx: &BuilderContext<N>) -> Result<Self::ChunkStore>;
}

pub trait NetworkBuilder<N: NodeTypes>: Send {
    type Network: NetworkClient<ChunkSet = N::ChunkSet>;
    async fn build(self, ctx: &BuilderContext<N>) -> Result<Self::Network>;
}
```

This allows swapping implementations (e.g., different storage backends) while maintaining type safety.

## See Also

- [Node Types](node-types.md) - Detailed explanation of node type hierarchy
- [Chunks](chunks.md) - Chunk architecture and storage
- [Swarm API](../swarm/api.md) - Protocol trait definitions
- [Client Architecture](../client/architecture.md) - libp2p integration layer
