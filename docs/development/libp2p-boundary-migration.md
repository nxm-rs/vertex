# libp2p Boundary Migration Plan

This document outlines the migration plan to restructure the vertex crates so that the libp2p boundary is correctly placed in `vertex-swarm-client`, keeping all other `vertex-swarm-*` crates libp2p-agnostic.

## Overview

The goal is to ensure:
- `vertex-swarm-api` and `vertex-swarm-core` are **libp2p-free**
- `vertex-swarm-client` is the **libp2p boundary** where all libp2p types are confined
- Higher-level crates work with abstract traits, enabling testing and alternative transports

## Current State

### vertex-swarm-core (NEEDS CHANGE)

**Location:** `crates/swarm/core/`
**Problem:** Contains libp2p code that should be in vertex-swarm-client

**Files with libp2p imports:**
- `behaviour.rs` - `NodeBehaviour` composed libp2p behaviour
- `bootnodes.rs` - bootnode connection logic
- `node.rs` - `SwarmNode` wrapping `libp2p::Swarm`
- `lib.rs` - re-exports

**Files that are libp2p-free:**
- `service.rs` - `ClientService` event processing
- `stats.rs` - statistics collection
- `config.rs` - configuration
- `constants.rs` - constants

### vertex-swarm-client (TARGET FOR libp2p CODE)

**Location:** `crates/swarm/client/`
**Current state:** libp2p-free (wrong!)

**Current content:**
- `Client` - high-level client implementing `SwarmClient` trait
- Re-exports from `vertex-swarm-core`

### vertex-swarm-api (NO CHANGE NEEDED)

**Location:** `crates/swarm/api/`
**Status:** Already libp2p-free
**Contains:** Core traits (`SwarmClient`, `Topology`, etc.)

### vertex-node-core (NO CHANGE NEEDED)

**Location:** `crates/node/core/`
**Status:** Already generic infrastructure (no libp2p, no swarm-specific)
**Contains:** Logging, config, CLI args, version info

## Migration Steps

### Phase 1: Prepare vertex-swarm-client

1. Add libp2p dependencies to vertex-swarm-client
2. Create module structure:
   ```
   crates/swarm/client/src/
   ├── lib.rs
   ├── client.rs          (existing - SwarmClient)
   ├── node/
   │   ├── mod.rs
   │   ├── behaviour.rs   (from swarm-core)
   │   ├── builder.rs     (from swarm-core/node.rs)
   │   └── swarm.rs       (from swarm-core/node.rs)
   ├── service.rs         (from swarm-core)
   ├── bootnodes.rs       (from swarm-core)
   └── stats.rs           (from swarm-core)
   ```

### Phase 2: Move Code

| Source | Destination | Notes |
|--------|-------------|-------|
| `swarm-core/src/behaviour.rs` | `swarm-client/src/node/behaviour.rs` | NodeBehaviour |
| `swarm-core/src/node.rs` | `swarm-client/src/node/` (split) | SwarmNode, SwarmNodeBuilder |
| `swarm-core/src/bootnodes.rs` | `swarm-client/src/bootnodes.rs` | BootnodeProvider |
| `swarm-core/src/service.rs` | `swarm-client/src/service.rs` | ClientService |
| `swarm-core/src/stats.rs` | `swarm-client/src/stats.rs` | Stats collection |

### Phase 3: Update vertex-swarm-core

1. Remove libp2p-dependent code (now in swarm-client)
2. Remove libp2p dependencies from Cargo.toml
3. Keep only orchestration/business logic
4. Add dependency on vertex-swarm-client
5. Re-export types from swarm-client for API stability

### Phase 4: Update Dependents

- Update vertex-swarm-builder
- Update vertex-swarm-node
- Update vertex binary

### Phase 5: Cleanup

- Remove dead code
- Update documentation
- Run full test suite

## Dependency Graph After Migration

```
vertex (binary)
└── vertex-swarm-node
    ├── vertex-swarm-core (libp2p-free orchestration)
    │   ├── vertex-swarm-api (traits)
    │   └── vertex-swarm-client (libp2p impl) <- NEW DEPENDENCY
    │       ├── vertex-swarm-kademlia
    │       ├── vertex-swarm-peermanager
    │       └── vertex-net-*
    └── vertex-node-builder
        └── vertex-node-core
```

## API Changes

### Breaking Changes (if not using re-exports)

- `vertex_swarm_core::SwarmNode` -> `vertex_swarm_client::SwarmNode`
- `vertex_swarm_core::NodeBehaviour` -> `vertex_swarm_client::NodeBehaviour`
- `vertex_swarm_core::ClientService` -> `vertex_swarm_client::ClientService`

### Mitigation

Add re-exports in `vertex-swarm-core/src/lib.rs`:
```rust
// Re-export from swarm-client for backward compatibility
pub use vertex_swarm_client::{
    SwarmNode, SwarmNodeBuilder, NodeBehaviour,
    ClientService, ClientHandle, ClientCommand, ClientEvent,
    // etc.
};
```

## Risks and Considerations

1. **Circular dependencies**: Ensure no cycles are introduced
   - `swarm-api` must not depend on `swarm-client`
   - `swarm-client` depends on `swarm-api` (to implement traits)
   - `swarm-core` depends on both

2. **Feature flags**: The `cli` feature in swarm-core may need adjustment

3. **Compilation time**: More crates = potentially longer initial compile, but better incremental builds

4. **Test coverage**: Ensure tests move with the code

## Benefits

- **Mock implementations**: With traits in `swarm-api`, can create mock impls for testing
- **Alternative transports**: Could create `vertex-client-waku` or similar
- **WASM support**: `swarm-core` could potentially compile to WASM (no libp2p)

## See Also

- [Client Architecture](../client/architecture.md) - Target architecture
- [Crate Migration](crate-migration.md) - Overall crate restructuring
