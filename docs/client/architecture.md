# Client Layer Architecture

The client layer bridges libp2p networking with Swarm's overlay network. This is where the libp2p boundary is defined.

## Abstraction Boundary

```
┌─────────────────────────────────────────────────────────┐
│                     Client Layer                        │
│  (vertex-swarm-client)                                  │
│                                                         │
│  Types: OverlayAddress, BzzAddress, SwarmNode           │
│  Responsibility: PeerId <-> OverlayAddress mapping      │
└─────────────────────────────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────┐
│                   Network Layer                         │
│  (vertex-net-*)                                         │
│                                                         │
│  Types: PeerId, Multiaddr, ConnectionId                 │
│  Responsibility: libp2p behaviour, protocol handlers    │
└─────────────────────────────────────────────────────────┘
```

## Key Principle

**Network crates use libp2p types only.** They have no knowledge of Swarm overlay addresses.

The client layer owns the mapping between:
- `PeerId` (libp2p transport identity)
- `OverlayAddress` (Swarm network address derived from ethereum key)

## Target Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  vertex-node-core                                               │
│  (generic node infrastructure - logging, config, CLI args)      │
│  NO libp2p, NO swarm-specific logic                             │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│  vertex-swarm-core                                              │
│  (swarm domain logic, orchestration)                            │
│  NO libp2p - uses abstract traits from vertex-swarm-api         │
│  - High-level node lifecycle                                    │
│  - Business rules (pricing decisions, peer selection strategy)  │
│  - Coordination between components                              │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│  vertex-swarm-api                                               │
│  (trait definitions - libp2p-free)                              │
│  - SwarmClient, Topology                                        │
│  - BandwidthAccounting, LocalStore, ChunkSync                   │
│  - BootnodeTypes, ClientTypes, StorerTypes                      │
└─────────────────────────────────────────────────────────────────┘
                              ▲
                              │ implements
┌─────────────────────────────────────────────────────────────────┐
│  vertex-swarm-client                                            │
│  (libp2p adapter layer - THE BOUNDARY)                          │
│  HAS libp2p - implements swarm-api traits                       │
│  - SwarmNode wrapping libp2p::Swarm                             │
│  - NodeBehaviour (composed libp2p behaviour)                    │
│  - ClientService (network event processing)                     │
│  - PeerId <-> OverlayAddress translation                        │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│  vertex-net-*                                                   │
│  (raw libp2p protocol implementations)                          │
│  - vertex-net-handshake, vertex-net-retrieval                   │
│  - vertex-net-pushsync, vertex-net-pricing                      │
│  - vertex-net-hive, vertex-net-topology                         │
└─────────────────────────────────────────────────────────────────┘
```

## Event Flow

```
Handler (protocol I/O)
    │
    ▼ HandlerEvent
Behaviour (connection management)
    │
    ▼ TopologyEvent (libp2p types)
Client (vertex-swarm-client)
    │
    ▼ Maps PeerId -> OverlayAddress
Application
```

## Why This Boundary?

1. **Testability** - Swarm logic can be tested without libp2p mocking
2. **Reusability** - Network behaviour works with any identity scheme
3. **Clarity** - Clear ownership of the PeerId <-> Overlay mapping
4. **Future-proofing** - Could support alternative transports (WASM, QUIC-only, etc.)

## libp2p Boundary Crate

`vertex-swarm-peer` is the designated **libp2p boundary crate** where `libp2p::Multiaddr` is permitted. This crate provides:

- `SwarmPeer` - Canonical peer identity type containing multiaddrs
- Multiaddr serialization utilities (Bee-compatible format)
- Signature verification and overlay address validation

Types that need to use `Multiaddr` should be defined in this crate rather than scattering libp2p dependencies across the codebase.

## Dependency Graph

```
vertex (binary)
└── vertex-swarm-node
    ├── vertex-swarm-core (libp2p-free orchestration)
    │   ├── vertex-swarm-api (traits)
    │   └── vertex-swarm-client (libp2p impl)
    │       ├── vertex-swarm-kademlia
    │       ├── vertex-swarm-peermanager
    │       └── vertex-net-*
    └── vertex-node-builder
        └── vertex-node-core
```

## See Also

- [Architecture Overview](../architecture/overview.md) - High-level crate organization
- [Swarm API](../swarm/api.md) - Protocol trait definitions
- [libp2p Boundary Migration](../development/libp2p-boundary-migration.md) - Migration plan for restructuring
