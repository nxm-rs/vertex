# Profiling Guide

CPU profiling, memory profiling, async runtime inspection, and key metrics for performance analysis in Vertex.

## Prerequisites

### jemalloc (optional)

The `jemalloc` feature requires system build tools:

```bash
# Ubuntu/Debian
sudo apt-get install build-essential autoconf

# Fedora/RHEL
sudo dnf install gcc autoconf automake

# macOS
xcode-select --install
```

If jemalloc fails to build, use the other profiling features which work without it.

## Quick Start

Enable profiling features when building:

```bash
# CPU profiling with pprof
cargo build --release --features profiling

# Memory profiling with jemalloc
cargo build --release --features jemalloc

# Async runtime inspection with tokio-console
cargo build --release --features tokio-console

# All profiling features
cargo build --release --features "profiling,jemalloc,tokio-console"
```

## CPU Profiling

### Generating Flamegraphs

When built with `--features profiling`, the metrics server exposes a pprof endpoint:

```bash
# Profile for 30 seconds and save flamegraph
curl "http://localhost:9191/debug/pprof/profile?seconds=30" > flamegraph.svg

# View in browser
open flamegraph.svg  # macOS
xdg-open flamegraph.svg  # Linux
```

### Interpreting Flamegraphs

- **Width** = time spent in function (wider = more CPU time)
- **Height** = call stack depth
- **Color** = function category (search supported)

Look for:
- Wide plateaus indicating hot functions
- Deep stacks indicating excessive nesting
- Unexpected functions taking significant time

### Tracing Spans

Key functions are instrumented with `#[tracing::instrument]` for fine-grained timing. Use with a tracing subscriber that exports to Jaeger/Tempo:

```bash
# Run with OTLP export enabled
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 vertex ...
```

## Memory Profiling

### jemalloc Setup

When built with `--features jemalloc`, jemalloc replaces the system allocator with additional statistics:

```bash
# Get current memory stats
curl http://localhost:9191/debug/memory
```

Response format:
```json
{
  "allocated": 12345678,
  "active": 23456789,
  "resident": 34567890,
  "mapped": 45678901,
  "retained": 5678901
}
```

### Memory Metrics Interpretation

| Metric | Description |
|--------|-------------|
| `allocated` | Bytes actively allocated by application |
| `active` | Bytes in active pages (may include fragmentation) |
| `resident` | RSS - total physical memory mapped |
| `mapped` | Total virtual memory mapped |
| `retained` | Bytes in retained (cached) pages |

**Key ratios:**
- `active / allocated` > 1.5 suggests fragmentation
- `resident / allocated` > 2.0 suggests memory pressure
- `retained` growing over time suggests memory not being returned to OS

## Async Runtime Inspection

### tokio-console Setup

1. Ensure `.cargo/config.toml` has `rustflags = ["--cfg", "tokio_unstable"]` (already configured in this workspace)
2. Build with `--features tokio-console`
3. Install tokio-console: `cargo install tokio-console`
4. Run vertex
5. Connect with console: `tokio-console`

**Note:** If you see a panic about "tokio_unstable", rebuild with:
```bash
RUSTFLAGS="--cfg tokio_unstable" cargo build --release --features tokio-console
```

### Using tokio-console

The console shows:
- **Tasks view**: All spawned tasks, their state, and runtime
- **Resources view**: Semaphores, mutexes, channels
- **Poll times**: How long each task poll takes

Look for:
- Tasks stuck in "Idle" for too long
- High poll times (> 1ms) indicating blocking in async code
- Many short-lived tasks indicating churn

## Key Metrics

All metrics are prefixed with `vertex_` when exported (e.g. `topology_depth` becomes `vertex_topology_depth`).

### Topology

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `topology_connected_peers` | Gauge | `node_type` | Connected peers by type (storer/client). Use `sum(topology_connected_peers)` for total. |
| `topology_known_peers_total` | Gauge | | Total known peers in routing table |
| `topology_depth` | Gauge | | Current Kademlia depth |
| `topology_connections_total` | Counter | `node_type`, `direction`, `outcome` | Connection attempts |
| `topology_connections_rejected_total` | Counter | `reason`, `direction` | Rejected connections |
| `topology_disconnections_total` | Counter | `reason`, `node_type` | Disconnections |
| `topology_connection_duration_seconds` | Histogram | `node_type` | Duration of closed connections |
| `topology_depth_increases_total` | Counter | | Depth increase events |
| `topology_depth_decreases_total` | Counter | | Depth decrease events |
| `topology_banned_peers` | Gauge | | Banned peer count |
| `topology_backoff_peers` | Gauge | | Peers in backoff |

### Dialing

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `topology_dial_failures_total` | Counter | `reason` | Failed dial attempts |
| `topology_dial_duration_seconds` | Histogram | `outcome` | Dial attempt duration |
| `topology_dial_addr_count` | Histogram | | Addresses attempted per dial |
| `topology_dial_exhausted_total` | Counter | | All addresses exhausted |
| `topology_pings_total` | Counter | `outcome` | Ping attempts |
| `topology_ping_rtt_seconds` | Histogram | | Ping round-trip time |

### Per-Bin Routing

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `topology_bin_connected_peers` | Gauge | `po` | Connected peers per proximity bin |
| `topology_bin_known_peers` | Gauge | `po` | Known peers per proximity bin |
| `topology_bin_dialing` | Gauge | `po` | Peers in dialing phase |
| `topology_bin_handshaking` | Gauge | `po` | Peers in handshake phase |
| `topology_bin_active` | Gauge | `po` | Active peers |
| `topology_bin_effective` | Gauge | `po` | Effective count (dialing+handshaking+active) |
| `topology_bin_target_peers` | Gauge | `po` | Target allocation (-1 for neighborhood) |
| `topology_bin_ceiling_peers` | Gauge | `po` | Max before rejecting inbound (-1 for neighborhood) |
| `topology_bin_nominal_peers` | Gauge | | Nominal floor (global, not per-bin) |

### Lock Contention

| Metric | Type | Description | Target |
|--------|------|-------------|--------|
| `topology_routing_phases_lock_seconds` | Histogram | Connection phases RwLock hold time | < 100us p99 |
| `topology_routing_candidates_lock_seconds` | Histogram | Candidates Mutex hold time | < 100us p99 |

### Poll Loop Performance

| Metric | Type | Description | Target |
|--------|------|-------------|--------|
| `topology_poll_duration_seconds` | Histogram | Time per poll loop iteration | < 1ms p99 |
| `topology_poll_events_total` | Counter | Events processed per poll | - |
| `topology_phase_transitions_total` | Counter | Connection phase transitions | - |

### Handshake

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `handshake_total` | Counter | `direction`, `purpose` | Total handshakes initiated |
| `handshake_success_total` | Counter | `direction`, `purpose`, `node_type` | Successful handshakes |
| `handshake_failure_total` | Counter | `direction`, `purpose`, `reason`, `stage` | Failed handshakes |
| `handshake_active` | Gauge | `direction` | Currently active handshakes |
| `handshake_stage` | Gauge | `direction`, `purpose`, `stage` | Handshakes in each stage |
| `handshake_duration_seconds` | Histogram | `direction`, `purpose`, `outcome`, `node_type` | Total handshake duration |
| `handshake_stage_duration_seconds` | Histogram | `direction`, `purpose`, `stage` | Per-stage duration |

### Hive (Peer Discovery)

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `hive_exchanges_total` | Counter | `direction` | Total hive exchanges |
| `hive_exchange_outcomes_total` | Counter | `direction`, `outcome` | Exchange outcomes |
| `hive_exchange_duration_seconds` | Histogram | `direction`, `outcome` | Exchange duration |
| `hive_peers_per_exchange` | Histogram | `direction` | Peers per exchange |
| `hive_exchanges_active` | Gauge | `direction` | Currently active exchanges |
| `hive_peers_received_total` | Counter | `outcome` | Peers received (valid/invalid) |
| `hive_peers_sent_total` | Counter | | Peers sent |
| `hive_peer_validation_failures_total` | Counter | `reason` | Peer validation failures |
| `hive_errors_total` | Counter | `direction`, `reason` | Exchange errors |

### Peer Scoring

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `peer_manager_total_peers` | Gauge | | Total tracked peers |
| `peer_manager_banned_peers` | Gauge | | Banned peers |
| `peer_manager_score_distribution` | Gauge | `le` | Current peer count per score range |

Uses `le` (upper bound) labels for native Grafana heatmap compatibility: `-100`, `-50`, `-10`, `-1`, `0`, `1`, `5`, `10`, `25`, `50`, `75`, `100`, `+Inf` (13 non-cumulative buckets).

Updated event-driven via `ScoreObserver` callbacks; O(1) per score change rather than periodic O(n) iteration.

### Gossip Verification

| Metric | Type | Description |
|--------|------|-------------|
| `topology_gossip_pending` | Gauge | Pending verifications |
| `topology_gossip_in_flight` | Gauge | Active verification dials |
| `topology_gossip_tracked_gossipers` | Gauge | Unique gossipers tracked |

### Proximity Cache

| Metric | Type | Description |
|--------|------|-------------|
| `topology_proximity_cached_items` | Gauge | Cached proximity items |
| `topology_proximity_cache_valid` | Gauge | Cache validity (1.0/0.0) |
| `topology_proximity_generation` | Gauge | Cache generation counter |

## Histogram Bucket Configuration

Two histogram families have custom bucket ranges configured in `vertex-observability`:

| Metric Suffix | Buckets | Range |
|--------------|---------|-------|
| `handshake_duration_seconds` | 1ms, 5ms, 10ms, 25ms, 50ms, 100ms, 250ms, 500ms, 1s, 2.5s, 5s, 10s, 15s | 1ms - 15s |
| `stage_duration_seconds` | 0.1ms, 0.5ms, 1ms, 5ms, 10ms, 25ms, 50ms, 100ms, 250ms, 500ms, 1s, 2.5s | 0.1ms - 2.5s |

All other histograms use the default Prometheus buckets (0.005s to 10s).

## Troubleshooting

### High CPU Usage

1. Generate a flamegraph to identify hot spots
2. Check `topology_poll_duration_seconds` histogram
3. Look for lock contention in `*_lock_seconds` metrics
4. Review tokio-console for blocking operations

### Memory Growth

1. Check `allocated` vs `active` ratio for fragmentation
2. Look for connection/peer count growth
3. Check gossip verifier queue depth

### Lock Contention

1. Monitor `topology_routing_*_lock_seconds` histograms for p99 spikes
2. Look for write lock contention (longer durations)
3. Consider if read operations dominate (RwLock appropriate)
4. Check for deadlock patterns in tokio-console

### Slow Handshakes

1. Check `handshake_duration_seconds` histogram
2. Review per-stage timing via `handshake_stage_duration_seconds`
3. Monitor gossip verification queue depth
4. Check for DNS resolution delays with bootnodes

## Prometheus Queries

### CPU Time per Poll

```promql
histogram_quantile(0.99, rate(vertex_topology_poll_duration_seconds_bucket[5m]))
```

### Lock Contention Trend

```promql
histogram_quantile(0.99, rate(vertex_topology_routing_phases_lock_seconds_bucket[5m]))
```

### Connection Churn

```promql
rate(vertex_topology_connections_total[5m])
```

### Peer Score Distribution

```promql
# Peers in each score range (heatmap-ready with numeric upper-bound labels)
vertex_peer_manager_score_distribution{instance=~"$instance"}

# Peers with very low scores (le < -10)
vertex_peer_manager_score_distribution{le=~"-100|-50|-10"}
```

### Handshake Latency

```promql
histogram_quantile(0.99, rate(vertex_handshake_duration_seconds_bucket{direction="outbound"}[5m]))
```

## Grafana Dashboard

A pre-built dashboard is available at `observability/grafana/provisioning/dashboards/json/vertex-overview.json`. It includes panels for:

- Network topology (depth, connected peers, bin distribution)
- Handshake performance (duration, success rate, stage breakdown)
- Peer scoring (score distribution, peers by score range)
- Hive exchange statistics
- Connection lifecycle (dial failures, disconnections)

To use with the local observability stack:

```bash
cd observability
docker compose up -d
# Dashboard available at http://localhost:3000
```
