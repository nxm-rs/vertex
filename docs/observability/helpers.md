# Observability Helpers

This document describes the observability helpers available for instrumenting Vertex components.

## Crate Structure

| Crate | Purpose |
|-------|---------|
| `vertex-observability` | Node-generic observability (guards, labels, macros, tracing, metrics server) |
| `vertex-swarm-observability` | Swarm protocol-specific labels and re-exports |

## LabelValue Trait + Strum Integration

The `LabelValue` trait provides type-safe conversion from enums to metric label strings. It integrates with [strum](https://docs.rs/strum) for zero-boilerplate support.

### Usage

```rust
use strum::IntoStaticStr;
use vertex_observability::LabelValue;

#[derive(IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ConnectionDirection {
    Inbound,   // → "inbound"
    Outbound,  // → "outbound"
}

// LabelValue is auto-implemented via blanket impl
let dir = ConnectionDirection::Inbound;
assert_eq!(dir.label_value(), "inbound");

// Use in metrics:
counter!("connections", "direction" => dir.label_value()).increment(1);
```

### Strum Attributes

```rust
// snake_case (most common for metrics)
#[derive(IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DisconnectReason {
    RemoteClosed,      // → "remote_closed"
    ConnectionError,   // → "connection_error"
}

// Custom values
#[derive(IntoStaticStr)]
pub enum NodeType {
    #[strum(serialize = "client")]
    Client,
    #[strum(serialize = "storer")]
    Storer,
}
```

## Common Labels

### Node-Generic (`vertex_observability::labels`)

```rust
use vertex_observability::labels::{direction, outcome, boolean, cache};

counter!("requests", "direction" => direction::INBOUND).increment(1);
counter!("operations", "outcome" => outcome::SUCCESS).increment(1);
gauge!("feature_enabled", "enabled" => boolean::from_bool(true)).set(1.0);
counter!("lookups", "result" => cache::HIT).increment(1);
```

### Swarm-Specific (`vertex_swarm_observability::labels`)

```rust
use vertex_swarm_observability::labels::{node_type, protocol, disconnect, transport};

counter!("connections", "node_type" => node_type::CLIENT).increment(1);
counter!("exchanges", "protocol" => protocol::HIVE).increment(1);
counter!("disconnects", "reason" => disconnect::REMOTE).increment(1);
```

## Drop-Based Guards

Guards ensure metrics are updated even on early returns or panics.

### GaugeGuard

Tracks active/in-flight operations:

```rust
use vertex_observability::GaugeGuard;

fn handle_request() {
    let _active = GaugeGuard::increment(gauge!("requests_active"));
    // gauge +1 now, -1 on drop

    process()?; // Even if this fails, gauge is decremented
}
```

### TimingGuard

Records operation duration:

```rust
use vertex_observability::TimingGuard;

fn process_item() {
    let _timing = TimingGuard::new(histogram!("process_duration_seconds"));
    // ... work ...
} // duration recorded on drop
```

### OperationGuard

Combined active gauge + finished counter:

```rust
use vertex_observability::OperationGuard;

fn handle_task() {
    let _guard = OperationGuard::new(
        gauge!("tasks_active"),
        counter!("tasks_finished_total"),
    );
    // On drop: gauge -1, counter +1
}
```

### CounterGuard

Increments counter on drop:

```rust
use vertex_observability::CounterGuard;

fn process() {
    let _done = CounterGuard::new(counter!("items_processed_total"));
    // ... work ...
} // counter +1 on drop
```

## Lazy Metric Macros

For static metrics that should initialize after the recorder is installed:

```rust
use std::sync::LazyLock;
use metrics::{Counter, Gauge, Histogram};
use vertex_observability::{lazy_counter, lazy_gauge, lazy_histogram};

static REQUESTS: LazyLock<Counter> = lazy_counter!("http_requests_total");
static ERRORS: LazyLock<Counter> = lazy_counter!("http_errors_total", "code" => "500");
static CONNECTIONS: LazyLock<Gauge> = lazy_gauge!("active_connections");
static LATENCY: LazyLock<Histogram> = lazy_histogram!("request_duration_seconds");

fn handle() {
    REQUESTS.increment(1);
    CONNECTIONS.increment(1.0);
    LATENCY.record(0.05);
}
```

## Protocol Metrics Pattern

Recommended pattern for protocol implementations:

```rust
use std::time::Instant;
use strum::IntoStaticStr;
use vertex_observability::{LabelValue, GaugeGuard, TimingGuard};
use vertex_swarm_observability::labels::protocol;

#[derive(IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ExchangeOutcome {
    Success,
    Timeout,
    CodecError,
    ProtocolError,
}

pub struct ProtocolMetrics {
    _active: GaugeGuard,
    _timing: TimingGuard,
    outcome_recorded: bool,
}

impl ProtocolMetrics {
    pub fn new(direction: &'static str) -> Self {
        counter!("hive_exchanges_total", "direction" => direction).increment(1);

        Self {
            _active: GaugeGuard::increment(
                gauge!("hive_exchanges_active", "direction" => direction)
            ),
            _timing: TimingGuard::new(
                histogram!("hive_exchange_duration_seconds", "direction" => direction)
            ),
            outcome_recorded: false,
        }
    }

    pub fn record_outcome(mut self, outcome: ExchangeOutcome) {
        counter!("hive_exchange_outcomes_total", "outcome" => outcome.label_value())
            .increment(1);
        self.outcome_recorded = true;
    }
}

impl Drop for ProtocolMetrics {
    fn drop(&mut self) {
        if !self.outcome_recorded {
            counter!("hive_exchange_outcomes_total", "outcome" => "unknown").increment(1);
        }
    }
}
```

## Re-exports

`vertex-swarm-observability` re-exports everything from `vertex-observability`:

```rust
// All of these work:
use vertex_swarm_observability::{
    LabelValue, GaugeGuard, TimingGuard, CounterGuard, OperationGuard,
    lazy_counter, lazy_gauge, lazy_histogram,
    labels as common,  // node-generic labels
    strum,             // for derives
    metrics_crate,     // metrics crate re-export
};
```
