# Configuration Architecture

This document describes the internal configuration architecture: how CLI arguments are parsed, validated, and assembled into the structs that node components consume.

For the user-facing CLI reference, see [CLI Configuration](../cli/configuration.md).

## Three-tier Pattern

Configuration flows through three layers: **Args** (CLI + serde), then **Config** (validated), then **ValidatedConfig** (assembled, node-type-specific).

1. **Args structs** (see `vertex_node_core::args`): flat, serialisable structs that derive both `clap::Args` and `serde::{Serialize, Deserialize}`. They carry raw user input and provide defaults. Each Args struct exposes one or more `*_config()` builder methods that produce validated config objects.

2. **Config structs** (various crates): validated, domain-specific configuration produced by Args builder methods. Examples include `OtlpConfig`, `OtlpLogsConfig`, `DatabaseConfig`, and `MetricsConfig`. These are protocol-agnostic and live close to the component that consumes them.

3. **ValidatedConfig structs** (see `vertex_swarm_builder::config`): fully assembled, node-type-specific configurations that hold runtime objects such as `Arc<Identity>` and `Arc<Spec>`. These are `BootnodeConfig`, `ClientConfig`, and `StorerConfig`.

## Naming Convention for Builder Methods

Builder methods on `Args` structs are named after the **output domain**, not the input struct. For example:

| Struct | Method | Produces | Rationale |
|--------|--------|----------|-----------|
| `TracingArgs` | `tracing_config()` | `OtlpConfig` | Named for the tracing domain |
| `TracingArgs` | `tracing_logs_config()` | `OtlpLogsConfig` | Named for the tracing-logs domain |
| `LogArgs` | `stdout_config()` | `StdoutConfig` | Specific name because `LogArgs` produces multiple sub-configs |
| `MetricsArgs` | `metrics_config()` | `MetricsConfig` | Named for the metrics domain |

When a struct produces a single config, the method name matches `<domain>_config()`. When it produces multiple configs (like `LogArgs` producing stdout and potentially file configs), each method uses a more specific name to avoid ambiguity.

## Why Aggregators Do Not Implement `NodeBuildsProtocol`

`InfraArgs` and `NodeArgs` aggregate multiple `Args` structs but do not implement the `NodeBuildsProtocol` trait (in `vertex_node_api`). This is by design: `NodeBuildsProtocol` uses associated types (`type Protocol`) that bind to a specific protocol implementation. The aggregator structs are protocol-agnostic and serve purely as CLI composition helpers.

## Crate Layering

The configuration types are split across crates to respect the protocol-agnostic/protocol-specific boundary:

- **`vertex_node_core`** contains `InfraConfig` and the Args structs. These are protocol-agnostic: they know nothing about swarm protocols, identity, or network topology.

- **`vertex_swarm_builder`** contains `BootnodeConfig`, `ClientConfig`, and `StorerConfig`. These are protocol-specific: they hold `Arc<Identity>`, `Arc<Spec>`, `NetworkConfig<KademliaConfig>`, and other swarm runtime objects. They live in the builder crate because that is where protocol arguments are resolved and assembled into a running node.

This separation is intentional. Moving the validated configs into `vertex_node_core` would force the core crate to depend on swarm-specific types, breaking the layering.

## NodeBuildsProtocol Delegation

The `NodeBuildsProtocol` trait (in `vertex_node_api`) provides a uniform interface for node-type-specific configuration. Each validated config struct implements `NodeBuildsProtocol`, which the swarm builder uses to select the correct protocol stack. The builder delegates to the config's getters (`spec()`, `identity()`, `network()`, `bandwidth()`, etc.) when constructing protocol components.

Methods like `identity()` on validated config structs return runtime objects (`Arc<Identity>`), not configuration structs; they are not renamed to `*_config()` because they are not config builders.

## See Also

- [CLI Configuration](../cli/configuration.md): user-facing CLI reference
- [Node Builder](node-builder.md): how configuration flows into the builder
- [Node Types](node-types.md): node type descriptions
