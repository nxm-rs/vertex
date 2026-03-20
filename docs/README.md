# Vertex Documentation

Vertex is a modular, high-performance implementation of the Ethereum Swarm protocol written in Rust.

## Documentation Structure

### Architecture

High-level design and crate organisation.

- [**Overview**](architecture/overview.md) - Crate structure, design principles, dependency flow
- [**Node Types**](architecture/node-types.md) - Bootnode, Client, Storer nodes
- [**Node Builder**](architecture/node-builder.md) - Type-state builder pattern
- [**Chunks**](architecture/chunks.md) - Chunk types, storage, authorisation

### Swarm Protocol

Swarm-specific protocol details and differences from Bee.

- [**API**](swarm/api.md) - Core traits (SwarmPrimitives, SwarmClientTypes, SwarmStorerTypes)
- [**Protocols**](swarm/protocols.md) - Network protocol patterns (headered streams, etc.)
- [**Protocol Errors**](protocol-errors.md) - Error handling architecture and conventions
- [**Hive Gossip**](swarm/hive-gossip.md) - Peer discovery gossip strategy
- [**Differences from Bee**](swarm/differences-from-bee.md) - Architectural improvements over Bee

### Client Layer

libp2p integration and the network abstraction boundary.

- [**Architecture**](client/architecture.md) - libp2p boundary, PeerId/OverlayAddress mapping

### Networking

Protocol-agnostic network utilities.

- [**Address Management**](networking/address-management.md) - Address classification, NAT, local network detection
- [**Peer Management**](networking/peer-management.md) - Arc-per-peer state, registry, scoring, events
- [**Peer Dialing Strategy**](networking/peer-dialing-strategy.md) - Bootstrapping, backoff, candidate selection

### CLI

Command-line interface and configuration.

- [**Configuration**](cli/configuration.md) - Configuration architecture, argument groups, quick start

### Observability

Production monitoring: metrics, tracing, and logging.

- [**Design**](observability/design.md) - Span boundaries, metrics patterns, naming conventions
- [**Helpers**](observability/helpers.md) - LabelValue trait, guards, macros, common labels
- [**Profiling**](observability/profiling.md) - CPU/memory profiling, async inspection, metrics reference
- [**Local Stack**](../observability/README.md) - Docker Compose setup for Prometheus, Tempo, Loki, Grafana

### Design Proposals

Internal design documents for planned changes.

- [**Chunk Size Const Generic**](design/chunk-size-const-generic.md) - Making chunk body size a compile-time const generic

### Development

Internal documentation for contributors.

- [**Bee Protocol Improvements**](development/bee-protocol-improvements.md) - Upstream suggestions

## Quick Links

| Topic | Document |
|-------|----------|
| What node mode should I run? | [Node Types](architecture/node-types.md) |
| How do I configure Vertex? | [CLI Configuration](cli/configuration.md) |
| How is the code organised? | [Architecture Overview](architecture/overview.md) |
| What's different from Bee? | [Differences from Bee](swarm/differences-from-bee.md) |
| How does bandwidth accounting work? | [Swarm API](swarm/api.md) |
| Where is libp2p used? | [Client Architecture](client/architecture.md) |
| How do I monitor my node? | [Observability Design](observability/design.md) |

For node types, crate dependencies, and architecture diagrams, see the [Architecture Overview](architecture/overview.md).

## See Also

- [Main README](../README.md) - Project overview, goals, and status
- [Bee Documentation](https://docs.ethswarm.org) - Official Swarm documentation
