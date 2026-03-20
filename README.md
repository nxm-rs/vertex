# Vertex

[![CI Status](https://github.com/nxm-rs/vertex/actions/workflows/unit.yml/badge.svg)](https://github.com/nxm-rs/vertex/actions/workflows/unit.yml)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)

**Swarm node that actually works. Built in Rust because Go was not cutting it for real decentralisation.**

## What is Vertex?

Vertex is a ground-up rewrite of the Ethereum Swarm node. Same protocol, different philosophy. We are building for modularity, performance, and the kind of reliability you would expect from infrastructure software.

Compatible with all Swarm protocols: postage stamps, push/pull sync, storage incentives, the works. If Bee does it, Vertex will do it faster.

## Architecture

Vertex is split into layered crates that can be used independently:

### Swarm Protocol

| Crate | Description |
|-------|-------------|
| `vertex-swarm-api` | Core protocol traits (topology, storage, bandwidth accounting) |
| `vertex-swarm-spec` | Network specification (`SwarmSpec` trait) |
| `vertex-swarm-forks` | Hardfork definitions (timestamp-based activation) |
| `vertex-swarm-primitives` | Core types (`OverlayAddress`, `SwarmNodeType`) |
| `vertex-swarm-identity` | Node identity and signing |
| `vertex-swarm-node` | `SwarmNode` behaviour and client handler |
| `vertex-swarm-builder` | Node construction and launch |
| `vertex-swarm-rpc` | gRPC service implementations |
| `vertex-swarm-test-utils` | Test fixtures and helpers |

### Swarm Peers

| Crate | Description |
|-------|-------------|
| `vertex-swarm-peer` | `SwarmPeer` type (libp2p boundary for `Multiaddr`) |
| `vertex-swarm-peer-manager` | Peer lifecycle management |
| `vertex-swarm-peer-score` | Peer scoring |
| `vertex-swarm-topology` | Kademlia DHT, peer discovery, neighbourhood management |

### Swarm Bandwidth

| Crate | Description |
|-------|-------------|
| `vertex-swarm-bandwidth` | Per-peer bandwidth handles, lock-free recording |
| `vertex-swarm-bandwidth-pricing` | Pricing strategy |
| `vertex-swarm-bandwidth-pseudosettle` | Pseudosettle provider |
| `vertex-swarm-bandwidth-chequebook` | Chequebook types |
| `vertex-swarm-bandwidth-swap` | SWAP settlement provider |

### Swarm Network Protocols

| Crate | Description |
|-------|-------------|
| `vertex-swarm-net-proto` | Protobuf message definitions |
| `vertex-swarm-net-handler-core` | Base handler types |
| `vertex-swarm-net-headers` | Headered protocol abstraction |
| `vertex-swarm-net-handshake` | Peer authentication (SYN/SYNACK/ACK) |
| `vertex-swarm-net-hive` | Peer discovery gossip |
| `vertex-swarm-net-identify` | libp2p identify integration |
| `vertex-swarm-net-pingpong` | Connection liveness |
| `vertex-swarm-net-pricing` | Price announcements |
| `vertex-swarm-net-pseudosettle` | Bandwidth settlement |
| `vertex-swarm-net-pushsync` | Chunk push/receipt |
| `vertex-swarm-net-retrieval` | Chunk request/response |
| `vertex-swarm-net-swap` | SWAP settlement protocol |

### Swarm Storage

| Crate | Description |
|-------|-------------|
| `vertex-swarm-localstore` | Storage configuration |
| `vertex-swarm-storer` | Storer node storage |
| `vertex-swarm-redistribution` | Storage incentives |

### Node Infrastructure

| Crate | Description |
|-------|-------------|
| `vertex-node-api` | `NodeProtocol`, `InfrastructureContext` traits |
| `vertex-node-builder` | Type-state builder framework |
| `vertex-node-commands` | CLI commands |
| `vertex-node-core` | CLI configuration and logging |

### Networking

| Crate | Description |
|-------|-------------|
| `vertex-net-codec` | Protobuf codec utilities |
| `vertex-net-dialer` | Dial request tracking |
| `vertex-net-dnsaddr` | DNS address resolution |
| `vertex-net-local` | Local network detection |
| `vertex-net-ratelimiter` | Rate limiting |
| `vertex-net-utils` | Network utilities |
| `vertex-net-peer-registry` | Peer registry |
| `vertex-net-peer-store` | Peer persistence |
| `vertex-net-peer-score` | Generic peer scoring |
| `vertex-net-peer-backoff` | Exponential backoff |

### Supporting Crates

| Crate | Description |
|-------|-------------|
| `vertex-rpc-core` | gRPC service traits |
| `vertex-rpc-server` | gRPC server implementation |
| `vertex-storage` | Storage abstraction |
| `vertex-storage-redb` | redb storage backend |
| `vertex-metrics` | Lightweight metric primitives |
| `vertex-observability` | Tracing, Prometheus, profiling |
| `vertex-tasks` | Task lifecycle management |

## Goals

1. **Modularity**: Every component is a library. Import what you need, build what you want.
2. **Performance**: Concurrent processing, zero-copy where possible, no GC pauses.
3. **Client Diversity**: More implementations means a more resilient network.
4. **Developer Experience**: Ergonomic APIs and actual documentation.

## Related Projects

- [`nectar`](https://github.com/nxm-rs/nectar): Low-level Swarm primitives (BMT, chunks, postage)
- [`apiary`](https://github.com/nxm-rs/apiary): Kurtosis package for spinning up test networks
- [`apiarist`](https://github.com/nxm-rs/apiarist): Stress testing and integration checks

## Status

Under active development. Not production ready yet, but getting there.

## Building

```bash
cargo build --release
```

## Contributing

We welcome contributions. Please read the [CLA](./CLA.md) before submitting PRs.

- Open an [issue](https://github.com/nxm-rs/vertex/issues) if something is broken
- Join the [Matrix space](https://matrix.to/#/#nexum:nxm.rs) to discuss development

## Licence

[AGPL-3.0-or-later](./LICENSE): because decentralised storage should stay decentralised.

## Warning

This is development software. It compiles, it runs tests, but it is not ready for your production workloads. Yet.
