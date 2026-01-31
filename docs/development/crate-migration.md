# Crate Migration Plan

This document tracks the migration plan for restructuring Vertex crates to follow the reth pattern.

## Current State Analysis

### Existing Crates

| Crate | Purpose | Status |
|-------|---------|--------|
| `swarmspec` | Network specification (ID, hardforks, bootnodes) | Complete |
| `primitives` | Core types (chunks, addresses) | Exists |
| `swarm-api` | Swarm protocol traits | Good traits, needs cleanup |
| `swarm-core` | Concrete node implementations | Obsolete, blocked on TODOs |
| `node/api` | Node type system, lifecycle | Parallel to swarm-api, needs consolidation |
| `node/core` | CLI entry point | Good structure |
| `net/*` | Network primitives | Good building blocks |

### Problems Identified

1. **Dual Node Type Systems**: `swarm-api::node` and `node-api::node` define parallel hierarchies
2. **swarm-core is Blocked**: Has TODOs for vertex-network, vertex-access crates that don't exist
3. **Type Mismatches**: Trait objects vs generics, async inconsistencies
4. **Unclear Boundaries**: What goes in swarm-api vs node-api?

## Target Architecture

```
vertex/crates/
├── primitives/           # Core types (chunks, addresses, peers)
├── swarmspec/            # Network specification (like chainspec)
│
├── swarm-api/            # Swarm PROTOCOL traits (not node traits)
│   ├── chunk.rs          # Chunk types and validation
│   ├── storage.rs        # ChunkStore trait
│   ├── retrieval.rs      # Retrieval protocol
│   ├── pushsync.rs       # Push sync protocol
│   ├── pullsync.rs       # Pull sync protocol
│   └── access.rs         # Access control (postage stamps)
│
├── node/
│   ├── types/            # NodeTypes trait
│   ├── api/              # FullNodeComponents, lifecycle, events
│   ├── builder/          # Component builders
│   └── core/             # CLI binary
│
├── net/
│   ├── primitives/       # Network types
│   ├── primitives-traits/
│   ├── codec/
│   ├── handshake/
│   ├── headers/
│   └── pricing/
│
└── storage/              # Storage implementations (future)
```

## Key Design Principles

### 1. NodeTypes Trait Pattern

Define a stateless trait that carries associated types:

```rust
// node/types/src/lib.rs
pub trait NodeTypes: Clone + Debug + Send + Sync + 'static {
    /// Network specification
    type Spec: SwarmSpec;
    /// Chunk types this node handles
    type ChunkSet: ChunkTypeSet;
    /// Storage backend type
    type Storage: Default + Send + Sync + 'static;
}
```

### 2. Layered Hierarchy

```
NodeTypes           (static types only)
    |
FullNodeTypes       (adds DB, Provider)
    |
FullNodeComponents  (adds stateful components)
```

### 3. Component Builder Traits

Separate builder trait for each concern:

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

### 4. Swarm-API is Protocol-Level

`swarm-api` defines the Swarm protocol, not node architecture:

- **ChunkStore** - How chunks are stored/retrieved locally
- **NetworkClient** - How chunks are fetched from network
- **RetrievalProtocol** - The retrieval request/response protocol
- **PushSyncProtocol** - How chunks are pushed to responsible nodes
- **AccessController** - Postage stamp validation

These are the "what" of Swarm, not the "how" of running a node.

### 5. Node-API is Architecture-Level

`node/api` defines how a node is composed:

- **NodeTypes** - Type configuration
- **FullNodeComponents** - Component container
- **NodeLifecycle** - Start/stop/restart
- **EventDispatcher** - Event handling
- **TaskExecutor** - Background tasks

## Migration Steps

### Phase 1: Create node/types

Create a minimal `node/types` crate with the `NodeTypes` trait.

### Phase 2: Clean up swarm-api

Remove node-related traits from swarm-api. Keep only protocol traits.

**Keep:**
- `ChunkStore`, `ChunkIndex` (storage protocol)
- `NetworkClient`, `Discovery` (network protocol)
- `AccessController`, `Authenticator`, `Authorizer` (access protocol)
- `BandwidthController`, `BandwidthAccountant` (bandwidth protocol)
- Protocol handlers (retrieval, pushsync, pullsync)

**Remove/Move:**
- `SwarmBaseNode`, `SwarmFullNode`, `SwarmIncentivizedNode` -> DELETE (replaced by NodeTypes)
- `NodeMode` enum -> DELETE
- Any node orchestration logic -> node/api

### Phase 3: Consolidate node/api

Update node/api to use the new NodeTypes from node/types.

### Phase 4: Create node/builder

Create component builders following reth pattern.

### Phase 5: Delete swarm-core

The swarm-core crate responsibilities move to:
- Node types -> `node/types`
- Component composition -> `node/api` + `node/builder`
- Concrete implementations -> Future crates

### Phase 6: Update node/core CLI

Update the CLI to use the new builder pattern.

## Success Criteria

1. `cargo check --workspace` passes
2. `swarm-core` deleted
3. Single NodeTypes hierarchy (no parallel systems)
4. Clear separation: protocol traits (swarm-api) vs node architecture (node/*)
5. Builder pattern enables component replacement
6. Ready for implementing concrete storage/network crates

## See Also

- [Architecture Overview](../architecture/overview.md) - Target architecture
- [libp2p Boundary Migration](libp2p-boundary-migration.md) - Related migration
