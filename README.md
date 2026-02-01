# Vertex

[![CI Status](https://github.com/nxm-rs/vertex/actions/workflows/unit.yml/badge.svg)](https://github.com/nxm-rs/vertex/actions/workflows/unit.yml)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)

**Swarm node that actually works. Built in Rust because Go was not cutting it for real decentralisation.**

## What is Vertex?

Vertex is a ground-up rewrite of the Ethereum Swarm node. Same protocol, different philosophy. We are building for modularity, performance, and the kind of reliability you would expect from infrastructure software.

Compatible with all Swarm protocols: postage stamps, push/pull sync, storage incentives, the works. If Bee does it, Vertex will do it faster.

## Architecture

Vertex is split into layered crates that can be used independently:

### Node Layer
| Crate | Description |
|-------|-------------|
| `vertex-node-api` | Protocol lifecycle traits and node configuration |
| `vertex-node-types` | Infrastructure types (database, RPC, executor) |
| `vertex-node-core` | Node implementation with CLI and configuration |
| `vertex-node-builder` | Type-state builder for node construction |

### Swarm Layer
| Crate | Description |
|-------|-------------|
| `vertex-swarm-api` | Swarm protocol traits (topology, storage, sync) |
| `vertex-swarm-primitives` | Core types, addresses, chunk handling |
| `vertex-swarm-identity` | Cryptographic identity and handshake |
| `vertex-swarm-kademlia` | Kademlia DHT implementation |
| `vertex-swarm-bandwidth` | SWAP-compatible bandwidth accounting |
| `vertex-swarm-topology` | Peer discovery and neighbourhood management |
| `vertex-swarm-localstore` | Local chunk storage |
| `vertex-swarm-storer` | Full storer node implementation |
| `vertex-swarm-node` | Client node for upload/download |

### Network Layer
| Crate | Description |
|-------|-------------|
| `vertex-net-p2p` | libp2p networking stack |
| `vertex-net-primitives` | Network addressing and peer types |

### Supporting Crates
| Crate | Description |
|-------|-------------|
| `vertex-rpc` | JSON-RPC server implementation |
| `vertex-metrics` | Prometheus metrics |
| `vertex-tasks` | Async task management |

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
