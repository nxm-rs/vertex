# AGENTS: crates/node/

Generic node infrastructure shared by every protocol that wants to be a vertex node. This area knows nothing about Swarm or libp2p: it provides the lifecycle trait, the type-state builder, the launch context, and the CLI runner.

Root-level rules in `/AGENTS.md` apply here too. The notes below are the area-specific overlay.

## Crates

- `vertex-node-api`: `NodeProtocol`, `NodeBuildsProtocol`, `InfrastructureContext`, `NodeProtocolConfig`, `NodeRpcConfig`. The contract a protocol implements. `NodeProtocol::ServeView` plus `serve_view` is the transport seam: a protocol projects its components into a transport-specific view (the Swarm protocol wraps them in a gRPC adapter) that the builder registers. `ServeView` is left unbounded here because node-api sits below `vertex-rpc-server` and cannot name `ServeWith`.
- `vertex-node-core`: CLI args, config loading, data directories, version info. The generic infrastructure layer.
- `vertex-node-builder`: type-state builder and launch wiring. `LaunchContext<A>` is the one launch-stage type: it carries the executor, data dirs, database config, the `NodeRpcConfig` `A`, and the optional metrics attachment. `WithProtocol::launch_with<Tr>` builds the protocol, registers `P::serve_view(&components)` through the transport, and returns a `NodeHandle` holding the bare components. The metrics stage (`with_metrics`/`start_metrics_server`, which install the Prometheus recorder and the axum server) is gated behind the `metrics` feature, off by default (`default = []`), so the embedded FFI client and node-commands launch through the shell without pulling the exporter stack into their cone; the binary opts into `metrics` explicitly.
- `vertex-node-commands`: `run_cli` plus the `HasLogs`/`HasTracing` glue for any vertex binary.

## Dos

- Keep the surface protocol-agnostic. The Swarm-specific builder lives in `vertex-swarm-builder`; do not move Swarm types into the node crates.
- Plumb tasks through `vertex_tasks::TaskExecutor` and the `InfrastructureContext`. No raw `tokio::spawn` outside the executor.
- Surface config errors as `thiserror` enums with `IntoStaticStr`.
- When adding a new CLI arg, place it in `vertex-node-core::args` if it is protocol-agnostic, otherwise in the protocol's own args crate (for example `vertex-swarm-identity::args`).

## Donts

- Do not depend on `vertex-swarm-*` from `node-api`, `node-core`, `node-builder`, or `node-commands`. Reversing that dependency direction defeats the point of the split.
- Do not import `libp2p` here. The libp2p boundary is in `vertex-swarm-node`.
- Do not add another global runtime. The binary owns the tokio runtime; the builder consumes a handle.
- Do not leak `eyre::Report` through library APIs. `eyre` is for the CLI edge; libraries return structured errors.

## Tests

- `cargo test -p vertex-node-api` etc. per crate.
- Builder coverage is in `vertex-node-builder`'s tests plus the integration test that the binary exercises during `cargo test -p vertex`.
