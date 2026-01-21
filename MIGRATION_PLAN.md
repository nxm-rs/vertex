# Vertex Crate Migration Plan

## Current State Analysis

### Existing Crates

| Crate | Purpose | Status |
|-------|---------|--------|
| `swarmspec` | Network specification (ID, hardforks, bootnodes) | ✅ Complete |
| `primitives` | Core types (chunks, addresses) | ✅ Exists |
| `swarm-api` | Swarm protocol traits | ⚠️ Good traits, needs cleanup |
| `swarm-core` | Concrete node implementations | ❌ Obsolete, blocked on TODOs |
| `node/api` | Node type system, lifecycle | ⚠️ Parallel to swarm-api, needs consolidation |
| `node/core` | CLI entry point | ✅ Good structure |
| `net/*` | Network primitives | ✅ Good building blocks |

### Problems Identified

1. **Dual Node Type Systems**: `swarm-api::node` and `node-api::node` define parallel hierarchies
2. **swarm-core is Blocked**: Has TODOs for vertex-network, vertex-access crates that don't exist
3. **Type Mismatches**: Trait objects vs generics, async inconsistencies
4. **Unclear Boundaries**: What goes in swarm-api vs node-api?

## Target Architecture (Following Reth)

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
│   ├── types/            # NodeTypes trait (NEW)
│   ├── api/              # FullNodeComponents, lifecycle, events
│   ├── builder/          # Component builders (NEW)
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

## Key Design Principles (from Reth)

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
    ↓
FullNodeTypes       (adds DB, Provider)
    ↓
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

### Phase 1: Create node/types (NEW)

Create a minimal `node/types` crate with the `NodeTypes` trait:

```rust
// crates/node/types/src/lib.rs
pub trait NodeTypes: Clone + Debug + Send + Sync + Unpin + 'static {
    type Spec: SwarmSpec;
    type ChunkSet: ChunkTypeSet;
}

pub trait FullNodeTypes: NodeTypes {
    type ChunkStore: ChunkStore<ChunkSet = Self::ChunkSet>;
    type NetworkClient: NetworkClient<ChunkSet = Self::ChunkSet>;
    type AccessController: AccessController;
    type BandwidthController: BandwidthController;
}
```

### Phase 2: Clean up swarm-api

Remove node-related traits from swarm-api. Keep only protocol traits:

**Keep:**
- `ChunkStore`, `ChunkIndex` (storage protocol)
- `NetworkClient`, `Discovery` (network protocol)
- `AccessController`, `Authenticator`, `Authorizer` (access protocol)
- `BandwidthController`, `BandwidthAccountant` (bandwidth protocol)
- Protocol handlers (retrieval, pushsync, pullsync)

**Remove/Move:**
- `SwarmBaseNode`, `SwarmFullNode`, `SwarmIncentivizedNode` → DELETE (replaced by NodeTypes)
- `NodeMode` enum → DELETE
- Any node orchestration logic → node/api

### Phase 3: Consolidate node/api

Update node/api to use the new NodeTypes from node/types:

- Import `NodeTypes`, `FullNodeTypes` from `node/types`
- Remove duplicate trait definitions
- Keep lifecycle, events, tasks, exit management
- Update `FullNodeComponents` to use the trait hierarchy

### Phase 4: Create node/builder (NEW)

Create component builders following reth pattern:

```rust
// crates/node/builder/src/lib.rs
pub struct NodeBuilder<N: NodeTypes, State = ()> {
    config: NodeConfig,
    spec: Arc<N::Spec>,
    _state: PhantomData<State>,
}

impl<N: NodeTypes> NodeBuilder<N, ()> {
    pub fn with_storage<B: ChunkStoreBuilder<N>>(
        self,
        builder: B
    ) -> NodeBuilder<N, WithStorage<B>> { ... }
}
```

### Phase 5: Delete swarm-core

The swarm-core crate is obsolete. Its responsibilities are now:

- Node types → `node/types`
- Component composition → `node/api` + `node/builder`
- Concrete implementations → Future crates (vertex-storage, etc.)

### Phase 6: Update node/core CLI

Update the CLI to use the new builder pattern:

```rust
// Pseudocode for bin/vertex/main.rs
async fn main() {
    let config = NodeConfig::parse();

    NodeBuilder::new(config)
        .with_types::<MainnetNode>()
        .with_storage(DefaultStorageBuilder)
        .with_network(DefaultNetworkBuilder)
        .launch()
        .await?
        .wait_for_exit()
        .await
}
```

## File Changes Summary

### New Files
- `crates/node/types/Cargo.toml`
- `crates/node/types/src/lib.rs`
- `crates/node/builder/Cargo.toml`
- `crates/node/builder/src/lib.rs`
- `crates/node/builder/src/context.rs`
- `crates/node/builder/src/components/*.rs`

### Files to Modify
- `crates/swarm-api/src/node.rs` → DELETE entirely
- `crates/swarm-api/src/lib.rs` → Remove node exports
- `crates/node/api/src/node.rs` → Use NodeTypes from node/types
- `crates/node/api/src/builder.rs` → Move to node/builder or simplify
- `Cargo.toml` → Add new workspace members

### Files to Delete
- `crates/swarm-core/` → Entire crate

## Dependency Graph (Target)

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

## Success Criteria

1. `cargo check --workspace` passes
2. `swarm-core` deleted
3. Single NodeTypes hierarchy (no parallel systems)
4. Clear separation: protocol traits (swarm-api) vs node architecture (node/*)
5. Builder pattern enables component replacement
6. Ready for implementing concrete storage/network crates
