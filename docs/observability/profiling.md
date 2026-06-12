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

When built with `--features profiling`, the metrics server exposes a pprof endpoint. The metrics server is enabled with `--metrics` and listens on `127.0.0.1:1637` by default (`--metrics.addr`, `--metrics.port`):

```bash
# Profile for 30 seconds and save flamegraph
curl "http://localhost:1637/debug/pprof/profile?seconds=30" > flamegraph.svg

# View in browser
open flamegraph.svg  # macOS
xdg-open flamegraph.svg  # Linux
```

These `/debug/*` routes are unauthenticated. Keep the metrics endpoint bound to localhost or behind a firewall (see the [Local Stack README](../../observability/README.md)).

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
vertex node --tracing --tracing.endpoint http://localhost:4317
```

## Memory Profiling

### jemalloc Setup

When built with `--features jemalloc`, jemalloc replaces the system allocator with additional statistics:

```bash
# Get current memory stats
curl http://localhost:1637/debug/memory
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
| `topology_depth` | Gauge | | Current Kademlia depth |
| `topology_connections_total` | Counter | `node_type`, `direction`, `outcome` | Connection attempts |
| `topology_connections_rejected_total` | Counter | `reason`, `direction` | Rejected connections |
| `topology_disconnections_total` | Counter | `connection_type`, `reason`, `node_type` | Disconnections (`connection_type` is `peer` or `unknown`; the `unknown` series omits `node_type`) |
| `topology_connection_duration_seconds` | Histogram | `node_type` | Duration of closed connections |
| `topology_depth_increases_total` | Counter | | Depth increase events |
| `topology_depth_decreases_total` | Counter | | Depth decrease events |
| `topology_early_disconnects_total` | Counter | `reason` | Post-handshake connections that failed quickly |

Banned-peer and backoff counts are not topology families: they are owned by the peer manager (`peer_manager_banned_peers`) and the dialer (`dial_tracker_banned_peers{purpose}`, `dial_tracker_backoff_peers{purpose}`). The known-peer total lives in the peer manager as `peer_manager_total_peers`.

### Dialing

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `topology_dial_failures_total` | Counter | `reason`, `error_type` | Failed dial attempts |
| `topology_dial_duration_seconds` | Histogram | `outcome` | Dial attempt duration |
| `topology_dial_addr_count` | Histogram | | Addresses attempted per dial |
| `topology_dial_exhausted_total` | Counter | | All addresses exhausted |
| `topology_dials_throttled_total` | Counter | | Dials skipped by the dial-rate throttle |
| `topology_pings_total` | Counter | `outcome` | Ping attempts |
| `topology_ping_rtt_seconds` | Histogram | | Ping round-trip time |

The dialer crate (`vertex-net-dialer`) tracks its own per-purpose state, where `purpose` distinguishes discovery dials from other dial reasons:

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `dial_tracker_pending` | Gauge | `purpose` | Peers queued to dial |
| `dial_tracker_in_flight` | Gauge | `purpose` | Dials currently in flight |
| `dial_tracker_backoff_peers` | Gauge | `purpose` | Peers in dial backoff |
| `dial_tracker_banned_peers` | Gauge | `purpose` | Peers dial-banned |
| `dial_tracker_banned_total` | Counter | `purpose` | Dial bans applied |
| `dial_tracker_backoff_recorded_total` | Counter | `purpose` | Backoff entries recorded |

### Connection Phase

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `topology_phase` | Gauge | `phase` | One-hot node lifecycle phase (1 for the active phase, 0 otherwise) |
| `topology_phase_changes_total` | Counter | `from`, `to` | Node lifecycle phase transitions (bootstrap/converging/stable) |
| `topology_gossip_rejected_total` | Counter | `reason` | Gossiped peers rejected (e.g. `not_dialable`) |

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
| `handshake_stage` | Gauge | `direction`, `purpose`, `stage` | Handshakes in each stage |
| `handshake_duration_seconds` | Histogram | `direction`, `purpose`, `outcome`, `node_type` | Total handshake duration |
| `handshake_stage_duration_seconds` | Histogram | `direction`, `purpose`, `stage` | Per-stage duration |

### Headered Protocols (Hive and Other Request-Response)

Exchange counts, outcomes, durations, and active-stream gauges are shared across every protocol that rides the header frame, distinguished by the `protocol` label. Hive exchange metrics surface here rather than under hive-specific names:

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `protocol_exchanges_total` | Counter | `protocol`, `direction` | Exchanges started |
| `protocol_exchange_outcomes_total` | Counter | `protocol`, `direction`, `outcome`, `reason` | Exchange outcomes |
| `protocol_exchange_duration_seconds` | Histogram | `protocol`, `direction` | Exchange duration |
| `protocol_streams_total` | Counter | `protocol`, `direction` | Streams opened |
| `protocol_streams_active` | Gauge | `protocol`, `direction` | Currently active streams |
| `protocol_upgrade_errors_total` | Counter | `protocol`, `direction`, `reason` | Stream upgrade errors |

### Hive (Peer Discovery)

Hive emits only peer-count and validation families; its exchange metrics are the `protocol_*` families above with `protocol="hive"`:

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `hive_peers_received_total` | Counter | `outcome` | Peers received (valid/invalid) |
| `hive_peers_sent_total` | Counter | | Peers sent |
| `hive_peers_per_exchange` | Histogram | `direction` | Peers per exchange |
| `hive_peers_discarded_total` | Counter | `reason` | Peer batches discarded (`bootnode_mode`, `rate_limited`, `verifier_rejected`) |
| `hive_rate_limited_total` | Counter | | Inbound exchanges rejected by the per-peer rate limiter |
| `hive_validation_cache_total` | Counter | `outcome` | Validation cache lookups (`cache_hit`/`miss`) |
| `hive_validation_duration_seconds` | Histogram | `direction` | Peer-record validation duration |
| `hive_peer_validation_failures_total` | Counter | `reason` | Peer validation failures |

### Identify

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `identify_received_total` | Counter | `purpose`, `agent_kind` | Identify responses received; `agent_kind` is a bounded classification (`bee`/`vertex`/`other`) of the remote agent string, never the raw attacker-controlled value |
| `identify_sent_total` | Counter | `purpose` | Identify responses sent |
| `identify_pushed_total` | Counter | `purpose` | Targeted identify pushes |
| `identify_error_total` | Counter | `purpose`, `kind` | Identify errors (`timeout`/`apply`) |
| `identify_duration_seconds` | Histogram | `purpose`, `direction`, `outcome` | Identify exchange duration |

### Peer Manager

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `peer_manager_total_peers` | Gauge | | Total tracked peers |
| `peer_manager_unverified_peers` | Gauge | | Known peers awaiting first-dial verification |
| `peer_manager_banned_peers` | Gauge | | Banned peers |
| `peer_manager_health` | Gauge | `state` | Peers in each health state |
| `peer_manager_score_distribution` | Gauge | `le` | Current peer count per score range |
| `peer_manager_tracked_ips` | Gauge | | Distinct remote IPs tracked for cycling detection |
| `peer_manager_reports_total` | Counter | `source`, `event`, `outcome` | Score reports processed |
| `peer_manager_ip_cycling_detections_total` | Counter | | Identity-cycling cap crossings detected |
| `peer_manager_admission_rejected_total` | Counter | | Discovered peers rejected at admission |
| `peer_manager_overlay_mismatch_removed_total` | Counter | | Peers removed for overlay mismatch |
| `peer_manager_gossip_timestamp_rejected_total` | Counter | `reason` | Gossiped records rejected on timestamp check |

`peer_manager_score_distribution` uses `le` (upper bound) labels for native Grafana heatmap compatibility: `-100`, `-50`, `-10`, `-1`, `0`, `1`, `5`, `10`, `25`, `50`, `75`, `100`, `+Inf` (13 non-cumulative buckets). It is updated event-driven on each score change rather than by periodic O(n) iteration.

### Gossip Intake

| Metric | Type | Description |
|--------|------|-------------|
| `topology_gossip_tracked_gossipers` | Gauge | Unique gossipers tracked |
| `topology_gossip_tracked_cooldowns` | Gauge | Overlays with an active record cooldown |

### Peer Registry Connections

| Metric | Type | Description |
|--------|------|-------------|
| `peer_registry_pending_connections` | Gauge | Connections established but not yet activated |
| `peer_registry_active_connections` | Gauge | Activated connections |

### Proximity Index

| Metric | Type | Description |
|--------|------|-------------|
| `topology_proximity_cached_items` | Gauge | Cached proximity items |
| `topology_proximity_generation` | Gauge | Cache generation counter |

### Storage (redb backend)

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `db_operations_total` | Counter | `table`, `operation`, `outcome` | Database operations (get/put/delete/clear/entries/keys/count/commit) |
| `db_operation_duration_seconds` | Histogram | `table`, `operation` | Per-operation duration |
| `db_tx_duration_seconds` | Histogram | `mode` | Transaction lifetime (`read`/`write`) |
| `db_tx_commit_duration_seconds` | Histogram | | Write-transaction commit duration |
| `db_entries` | Gauge | `table` | Entry count per table |
| `redb_file_size_bytes` | Gauge | | Database file size |
| `redb_stored_bytes` / `redb_metadata_bytes` / `redb_fragmented_bytes` | Gauge | `table` | Per-table byte breakdown (also exported as `*_total` aggregates without the label) |
| `redb_tree_height` / `redb_leaf_pages` / `redb_branch_pages` | Gauge | `table` | Per-table B-tree shape |
| `redb_cache_evictions_total` | Gauge | | Cumulative cache evictions |

### Task Executor

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `executor_spawn_critical_tasks_total` | Counter | `task` | Critical tasks spawned |
| `executor_spawn_regular_tasks_total` | Counter | `task` | Regular tasks spawned |
| `executor_spawn_regular_blocking_tasks_total` | Counter | `task` | Regular blocking tasks spawned |
| `executor_spawn_finished_*_tasks_total` | Counter | `task` | Finished-task counters (one per spawn family) |
| `executor_tasks_panicked_total` | Counter | `type` | Tasks that panicked |
| `executor_tasks_running` | Gauge | `type`, `task`, `graceful` | Currently running tasks |

## Histogram Bucket Configuration

Histograms only render as Prometheus histograms (with `_bucket` series) when a bucket configuration is registered for their name suffix. `bin/vertex` builds a `HistogramRegistry` from six crates and installs it with the Prometheus recorder (`bin/vertex/src/cli.rs`): `vertex-swarm-net-headers`, `vertex-swarm-topology`, `vertex-swarm-net-handshake`, `vertex-swarm-net-hive`, `vertex-swarm-net-identify`, and `vertex-storage-redb`. Each crate declares its suffixes as a `HISTOGRAM_BUCKETS` const next to its metrics.

The six reusable bucket presets (`DURATION_FINE`, `DURATION_NETWORK`, `DURATION_SECONDS`, `LOCK_CONTENTION`, `POLL_DURATION`, `CONNECTION_LIFETIME`) live in `vertex_metrics::buckets` and are re-exported from `vertex-observability`. A crate may also inline a bespoke bucket list when no preset fits.

Registered histogram suffixes (a registration matches any metric name ending in the suffix, so `handshake_stage_duration_seconds` matches the `stage_duration_seconds` registration):

| Suffix | Source crate | Buckets |
|--------|--------------|---------|
| `protocol_exchange_duration_seconds` | headers | 10ms - 30s (bespoke) |
| `handshake_duration_seconds` | handshake | `DURATION_SECONDS` |
| `stage_duration_seconds` | handshake | `DURATION_FINE` |
| `identify_duration_seconds` | identify | `DURATION_SECONDS` |
| `hive_validation_duration_seconds` | hive | `DURATION_FINE` |
| `hive_peers_per_exchange` | hive | 1 - 100 (bespoke counts) |
| `topology_connection_duration_seconds` | topology | `CONNECTION_LIFETIME` |
| `topology_dial_duration_seconds` | topology | `DURATION_NETWORK` |
| `topology_dial_addr_count` | topology | 1 - 50 (bespoke counts) |
| `topology_ping_rtt_seconds` | topology | 1ms - 5s (bespoke) |
| `topology_poll_duration_seconds` | topology | `POLL_DURATION` |
| `topology_routing_candidates_lock_seconds` | topology | `LOCK_CONTENTION` |
| `topology_routing_phases_lock_seconds` | topology | `LOCK_CONTENTION` |
| `db_operation_duration_seconds` | storage-redb | 10us - 1s (bespoke) |
| `db_tx_duration_seconds` | storage-redb | 10us - 1s (bespoke) |
| `db_tx_commit_duration_seconds` | storage-redb | 10us - 1s (bespoke) |

A histogram whose name matches no registered suffix is not given buckets, so the Prometheus exporter renders it as a summary (quantile series), not a default-bucket histogram. There is no implicit "0.005s to 10s default histogram" fallback for unregistered families. When adding a histogram that should produce `_bucket` series, register its suffix in the owning crate's `HISTOGRAM_BUCKETS` and wire the crate into the registry in `bin/vertex/src/cli.rs`.

## Troubleshooting

### High CPU Usage

1. Generate a flamegraph to identify hot spots
2. Check `topology_poll_duration_seconds` histogram
3. Look for lock contention in `*_lock_seconds` metrics
4. Review tokio-console for blocking operations

### Memory Growth

1. Check `allocated` vs `active` ratio for fragmentation
2. Look for connection/peer count growth
3. Check unverified peer count (`peer_manager_unverified_peers`)

### Lock Contention

1. Monitor `topology_routing_*_lock_seconds` histograms for p99 spikes
2. Look for write lock contention (longer durations)
3. Consider if read operations dominate (RwLock appropriate)
4. Check for deadlock patterns in tokio-console

### Slow Handshakes

1. Check `handshake_duration_seconds` histogram
2. Review per-stage timing via `handshake_stage_duration_seconds`
3. Check for DNS resolution delays with bootnodes

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
