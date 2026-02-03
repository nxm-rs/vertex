# Vertex Documentation

Vertex is a modular, high-performance implementation of the Ethereum Swarm protocol written in Rust.

## Documentation Structure

### Architecture

High-level design and crate organization.

- [**Overview**](architecture/overview.md) - Crate structure, design principles, dependency flow
- [**Node Types**](architecture/node-types.md) - Bootnode, Client, Storer nodes
- [**Chunks**](architecture/chunks.md) - Chunk types, storage, authorization

### Swarm Protocol

Swarm-specific protocol details and differences from Bee.

- [**API**](swarm/api.md) - Core traits (Identity, Topology, BandwidthAccounting)
- [**Protocols**](swarm/protocols.md) - Network protocol patterns (headered streams, etc.)
- [**Hive Gossip**](swarm/hive-gossip.md) - Peer discovery gossip strategy
- [**Differences from Bee**](swarm/differences-from-bee.md) - Architectural improvements over Bee

### Client Layer

libp2p integration and the network abstraction boundary.

- [**Architecture**](client/architecture.md) - libp2p boundary, PeerId/OverlayAddress mapping

### Networking

Protocol-agnostic network utilities.

- [**Address Management**](networking/address-management.md) - Address classification, NAT, local network detection
- [**Peer Management**](networking/peer-management.md) - Arc-per-peer state, registry, caching, events

### CLI

Command-line interface and configuration.

- [**Configuration**](cli/configuration.md) - CLI arguments, node configuration, examples

### Development

Internal documentation for contributors.

- [**Crate Migration**](development/crate-migration.md) - Plan for restructuring crates
- [**libp2p Boundary Migration**](development/libp2p-boundary-migration.md) - Moving libp2p to correct layer
- [**Bee Protocol Improvements**](development/bee-protocol-improvements.md) - Upstream suggestions

## Quick Links

| Topic | Document |
|-------|----------|
| What node mode should I run? | [Node Types](architecture/node-types.md) |
| How do I configure Vertex? | [CLI Configuration](cli/configuration.md) |
| How is the code organized? | [Architecture Overview](architecture/overview.md) |
| What's different from Bee? | [Differences from Bee](swarm/differences-from-bee.md) |
| How does bandwidth accounting work? | [Swarm API](swarm/api.md) |
| Where is libp2p used? | [Client Architecture](client/architecture.md) |

## Node Types

Vertex supports three node modes:

```
┌─────────────┐
│   Storer    │  Storage + staking (not yet implemented)
│  pullsync   │
│  localstore │
│  redistrib. │
└──────┬──────┘
       │
┌──────▼──────┐
│   Client    │  Retrieval + upload (default)
│  retrieval  │
│  pushsync   │
│  bandwidth  │
└──────┬──────┘
       │
┌──────▼──────┐
│  Bootnode   │  Topology only
│    hive     │
│  kademlia   │
│  pingpong   │
└─────────────┘
```

## Crate Dependencies

```
                    primitives
                        |
          +-------------+-------------+
          |             |             |
      swarmspec     swarm-api       net/*
          |             |             |
          +-------------+-------------+
                        |
                   node/types
                        |
                    node/api
                        |
                  node/builder
                        |
                   node/core (CLI)
```

## See Also

- [Main README](../README.md) - Project overview, goals, and status
- [Bee Documentation](https://docs.ethswarm.org) - Official Swarm documentation
