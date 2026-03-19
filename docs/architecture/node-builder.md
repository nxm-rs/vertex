# Node Builder Architecture

The `vertex-node-builder` crate provides the type-state builder pattern for launching Vertex nodes.

## Type-State Pattern

The builder uses a type-state pattern where each stage is a distinct type, ensuring compile-time correctness:

```text
NodeBuilder
  │
  ├── with_launch_context(executor, dirs, api)
  ▼
WithLaunchContext<A>
  │
  ├── with_protocol(config: impl NodeBuildsProtocol)
  ▼
WithProtocol<P, A>
  │
  ├── launch()
  ▼
NodeHandle<P::Components>
```

### Protocol Type Inference

The protocol type `P` is inferred from the config type via the `NodeBuildsProtocol` trait:

```rust
// Protocol type is inferred from config
let handle = NodeBuilder::new()
    .with_launch_context(executor, dirs, api_config)
    .with_protocol(my_light_config)  // SwarmLightProtocol inferred
    .launch()
    .await?;
```

## Launch Context

The `LaunchContext` provides infrastructure to protocols:

- **TaskExecutor**: Spawns background tasks as critical (node shuts down on panic)
- **DataDirs**: Persistent storage directories
- **API config**: gRPC address and port

It implements `InfrastructureContext` so protocols receive it directly during launch.

## State Accumulation (launch.rs)

The `Attached<L, R>` type enables accumulating state while preserving access to previous values:

```rust
// Attach metrics to the launch context
let ctx = (executor, dirs)
    .with_metrics(metrics_config)?
    .attach(additional_state);

// Access both via .left() and .right()
ctx.attachment().left();   // WithMetrics
ctx.attachment().right();  // additional_state
```

This pattern avoids losing access to earlier configuration as the launch sequence progresses.

## Service Lifecycle

1. **Protocol launch**: `NodeProtocol::launch()` builds components and spawns services
2. **gRPC registration**: Components implement `RegistersGrpcServices` to add their RPC methods
3. **Server spawn**: gRPC server spawned as critical task
4. **Shutdown**: `NodeHandle::wait_for_shutdown()` waits for signal or critical task panic

All services are spawned as "critical tasks" - if any panics, the node shuts down gracefully.

## Metrics Integration

Metrics are optionally attached via `LaunchContextExt`:

```rust
let ctx = (executor, dirs)
    .with_metrics(Some(metrics_config))?
    .start_metrics_server()
    .await?;
```

The Prometheus recorder is installed globally and the HTTP server exposes `/metrics`.
