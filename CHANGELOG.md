# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

First public release of Vertex, a Rust implementation of an Ethereum Swarm node. This initial cut ships a working client and storer on the live network, plus an embeddable client library that also runs in the browser. Later releases append their changes above this summary.

### Added

- Three build modes from one shared builder path: a bare-minimum Swarm client (the default build), a storer node (`--features storer`), and an embeddable FFI client library, which also targets `wasm32` for the browser.
- libp2p networking: the Swarm handshake, hive peer discovery, Kademlia topology and dialing, NAT and reachability detection, and peer scoring.
- Chunk transfer: retrieval, pushsync, and storer pullsync, served through a verifying chunk provider, with postage stamp validation and single-owner chunk support.
- Bandwidth accounting with pseudosettle (chain-free) and SWAP cheque settlement, plus on-chain chequebook cashout behind the chain feature.
- Storage backends: redb on native targets and an IndexedDB cache in the browser.
- A gRPC node API and native observability through tracing and metrics.
- A cross-platform release pipeline building Linux, macOS, and Windows binaries on x86_64 and aarch64 via cargo-dist, a multi-arch Docker image, and cargo-release with git-cliff for versioning.
