<p align="center">
  <img src=".github/banner.svg" alt="Nexum · vertex — Ethereum Swarm node in Rust" width="100%" />
</p>

A new **Ethereum Swarm** node implementation in Rust — modular, high-performance, Bee-compatible. Vertex aims to be the fastest Swarm client while being easy to run on consumer hardware, and to contribute meaningful client diversity to the network.

Nexum builds on Swarm for content-addressed storage of firewall rulesets, snapshots, and shared state. Vertex is how we run our own node infrastructure for that.

> **Pre-release** and under active development. Testnets and lab environments only.

Looking for the org overview? See **[github.com/nxm-rs](https://github.com/nxm-rs)**.

---

## Build from source

```bash
git clone https://github.com/nxm-rs/vertex
cd vertex
cargo build --release
```

Binary lands at `target/release/vertex`. CLI documentation lives under [`docs/`](./docs); operational guides are still in progress.

---

## Goals

1. **Modularity.** Every component is a library: well-tested, documented, benchmarked. Import the chunk store, a network protocol, or storage-incentives in isolation.
2. **Performance.** Concurrent processing, careful resource usage, no accidental synchronisation.
3. **Client diversity.** A second production-grade client makes the Swarm network more resilient.
4. **Developer experience.** Ergonomic APIs, useful errors, real docs. A CLI (`dipper`, modelled on `cast`) coming as a sibling repo.

---

## Sibling repos

Vertex isn't shipped in isolation. The Swarm work under [nxm-rs](https://github.com/nxm-rs) spans:

| Repo | Role |
|---|---|
| **[nectar](https://github.com/nxm-rs/nectar)** | Low-level Swarm primitives — chunks, addressing, postage |
| **[bee](https://github.com/nxm-rs/bee)** | Reference Go client (fork; we contribute upstream) |
| **[swarm-contracts](https://github.com/nxm-rs/swarm-contracts)** | Economic-layer contracts · Solady + Foundry |
| **[apiarist](https://github.com/nxm-rs/apiarist)** | In-network stress tester |
| **[apiary](https://github.com/nxm-rs/apiary)** | One-command stack: Reth + Bee + supporting services |
| **[SWIPs](https://github.com/nxm-rs/SWIPs)** | Swarm Improvement Proposals |

---

## Contributing

Open an issue before non-trivial PRs — pre-release codebase under heavy churn. Conventional Commits. `cargo fmt`, `cargo clippy -- -D warnings`. Tests for protocol changes are non-optional.

## Security

See [SECURITY.md](https://github.com/nxm-rs/.github/blob/main/SECURITY.md) or email `security@nxm.rs`.

## License

AGPL-3.0-or-later. See [LICENSE](./LICENSE).

```
●  AGPL-3.0  ·  pre-release  ·  bee-compatible
```
