# Observability Helpers

This document describes the observability helpers available for instrumenting Vertex components.

## Crate Structure

| Crate | Purpose |
|-------|---------|
| `vertex-observability` | Node-generic observability (guards, labels, macros, tracing, metrics server) |

## LabelValue Trait + Strum Integration

The `LabelValue` trait provides type-safe conversion from enums to metric label strings. It integrates with strum for zero-boilerplate support.

### Usage

Derive `IntoStaticStr` from strum on your enum and annotate it with `#[strum(serialize_all = "snake_case")]`. The `LabelValue` trait is automatically implemented via a blanket impl for any type that implements `Into<&'static str>`. Calling `.label_value()` on a variant returns the snake_case string (e.g., `ConnectionDirection::Inbound` yields `"inbound"`). These strings can be passed directly to metric label positions.

### Strum Attributes

| Attribute | Effect | Example |
|-----------|--------|---------|
| `#[strum(serialize_all = "snake_case")]` | Converts variants to snake_case | `RemoteClosed` becomes `"remote_closed"` |
| `#[strum(serialize = "custom")]` | Uses a custom string for a variant | `Client` becomes `"client"` |

The `snake_case` serialization is the most common choice for metric labels.

## Common Labels

### Node-Generic (`vertex_observability::labels`)

The `vertex_observability::labels` module provides pre-defined label constants organized by category:

| Module | Constants | Purpose |
|--------|-----------|---------|
| `direction` | `INBOUND`, `OUTBOUND` | Traffic direction |
| `outcome` | `SUCCESS`, `FAILURE` | Operation result |
| `boolean` | `from_bool(val)` function | Feature flags, enabled/disabled |
| `cache` | `HIT`, `MISS` | Cache lookup results |

### Swarm-Specific (`vertex_observability::labels`)

Additional label modules for swarm-layer metrics:

| Module | Constants | Purpose |
|--------|-----------|---------|
| `node_type` | `CLIENT`, `STORER` | Node classification |
| `protocol` | `HIVE`, `PUSHSYNC`, etc. | Protocol names |
| `disconnect` | `REMOTE`, `LOCAL`, etc. | Disconnect reasons |
| `transport` | Transport type constants | Network transport |

## Drop-Based Guards

Guards ensure metrics are updated even on early returns or panics. Each guard type wraps a metric handle and performs its action on drop.

| Guard | Constructor | On Creation | On Drop | Use Case |
|-------|-------------|-------------|---------|----------|
| `GaugeGuard` | `GaugeGuard::increment(gauge)` | Gauge +1 | Gauge -1 | Tracking active/in-flight operations |
| `TimingGuard` | `TimingGuard::new(histogram)` | Records start time | Records elapsed duration to histogram | Measuring operation duration |
| `OperationGuard` | `OperationGuard::new(gauge, counter)` | Gauge +1 | Gauge -1, Counter +1 | Combined active tracking and completion counting |
| `CounterGuard` | `CounterGuard::new(counter)` | Nothing | Counter +1 | Ensuring an event is counted even on panic |

All guards implement `Drop`, so the cleanup action runs regardless of how the scope exits (normal return, early `?` return, or panic).

## Lazy Metric Macros

For static metrics that should initialize after the recorder is installed, use the lazy metric macros. These produce `LazyLock` values that defer metric registration until first access, avoiding issues with recorder installation ordering.

| Macro | Produces | Example |
|-------|----------|---------|
| `lazy_counter!` | `LazyLock<Counter>` | `lazy_counter!("http_requests_total")` |
| `lazy_gauge!` | `LazyLock<Gauge>` | `lazy_gauge!("active_connections")` |
| `lazy_histogram!` | `LazyLock<Histogram>` | `lazy_histogram!("request_duration_seconds")` |

All macros accept optional label pairs as additional arguments (e.g., `lazy_counter!("http_errors_total", "code" => "500")`). Once initialized, use the standard `metrics` crate methods: `.increment(1)` for counters, `.increment(1.0)` / `.set(val)` for gauges, and `.record(val)` for histograms.

## Protocol Metrics Pattern

The recommended pattern for protocol implementations combines several helpers:

1. Define an outcome enum with `#[derive(IntoStaticStr)]` and `#[strum(serialize_all = "snake_case")]` to get type-safe label values.
2. Create a metrics struct that holds a `GaugeGuard` (for active exchange tracking), a `TimingGuard` (for duration recording), and an `outcome_recorded` flag.
3. In the constructor, increment the total exchanges counter and initialize the guards.
4. Provide a `record_outcome` method that emits the outcome counter and sets the flag.
5. Implement `Drop` so that if no outcome was explicitly recorded, a counter with outcome `"unknown"` is emitted.

This pattern ensures that active gauges, durations, and outcome counters are always consistent, even when exchanges are interrupted by errors or cancellation.
