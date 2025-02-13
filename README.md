# Vertex

[![CI status](https://github.com/nullisyz/vertex/actions/workflows/unit.yml/badge.svg)](https://github.com/nullisxyz/vertex/actions/workflows/unit.yml)][gh-ci]
[![codecov](https://codecov.io/gh/nullisxyz/vertex/graph/badge.svg?token=O56JVSX6AB)](https://codecov.io/gh/nullisxyz/vertex)][codecov]

**Modular, high-performance implementation of the Ethereum Swarm protocol**

<!-- [Logo placeholder]

**[Install](https://vertex.rs/installation) | [User Book](https://vertex.rs) | [Developer Docs](./docs) | [Crate Docs](https://vertex.rs/docs)**
-->

## What is Vertex?

Vertex (pronunciation: /ˈvɜːrtɛks/) is a new Ethereum Swarm node implementation focused on being user-friendly, highly modular, and blazing-fast. Vertex is written in Rust and is compatible with all Swarm protocols including postage stamps, push/pull syncing, and the full storage incentives system. Built and driven forward by [Nullis](https://github.com/nullisxyz), Vertex is licensed under the GNU Affero General Public License v3.0 (AGPL-3.0).

## Goals

As a full Ethereum Swarm node, Vertex will allow users to connect to the Swarm network and interact with decentralised storage. This includes uploading and downloading content, participating in the storage incentives system, and being a good network citizen. Building a successful Swarm node requires creating a high-quality implementation that is both secure and efficient, as well as being easy to use on consumer hardware. It also requires building a strong community of contributors who can help support and improve the software.

More concretely, our goals are:

1. **Modularity**: Every component of Vertex is built to be used as a library: well-tested, heavily documented and benchmarked. We envision that developers will import components like network protocols or chunk storage and build innovative solutions on top of them. The project is split into three main repositories:
   - `vertex`: The full node implementation
   - `nectar`: Core primitives and protocols specific to Ethereum Swarm
   - `dipper`: A CLI tool for interacting with Swarm (similar to `cast` in Foundry)

2. **Performance**: Vertex aims to be the fastest Swarm implementation. Written in Rust with a focus on concurrent processing and efficient resource usage, we strive to optimize every aspect from chunk processing to network communication.

3. **Client Diversity**: The Swarm network becomes more resilient when no single implementation dominates. By building a new client, we hope to contribute to Swarm's decentralisation and anti-fragility.

4. **Developer Experience**: Through great documentation, ergonomic APIs, and developer tooling like `dipper`, we want to make it easy for developers to build on Swarm.

## Status

Vertex is under active development and not yet ready for production use.

## Getting Help

If you have questions:

- Join the [Signal group](https://signal.group/#CjQKIHNV-kWphhtnpwS3zywC7LRr5BEW9Q1XyDl2qZtL2WYqEhAyO0c8tGmrQDmEsY15rALt) to discuss development with the Nullis team
- Open a [discussion](https://github.com/nullisxyz/vertex/discussions/new) with your question
- Open an [issue](https://github.com/nullisxyz/vertex/issues/new) to report a bug

## License

Vertex is licensed under the GNU Affero General Public License v3.0 (AGPL-3.0). See [LICENSE](./LICENSE) for details.

## Warning

This software is currently in development. While we strive for correctness, bugs may exist. Use at your own risk.
