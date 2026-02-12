# Observability

Production observability for Vertex nodes: metrics, tracing, and logging.

## Documentation

- [**Design**](design.md) - Observability methodology, span boundaries, metrics patterns, naming conventions
- [**Helpers**](helpers.md) - LabelValue trait, guards, macros, common labels

## Local Development Stack

The [observability stack](../../observability/README.md) provides a Docker Compose environment with:

- **Prometheus** (port 9099) - Metrics collection
- **Tempo** (port 4317/4318) - Distributed tracing via OTLP
- **Loki** (port 3100) - Log aggregation
- **Grafana** (port 3000) - Unified dashboards

Quick start:

```bash
cd observability
docker compose up -d
```

## CLI Configuration

### Metrics

```bash
# Enable metrics endpoint (default: 127.0.0.1:9191)
vertex node --metrics --metrics.port 9191

# With custom prefix
vertex node --metrics --metrics.prefix vertex_mainnet
```

### Tracing (OTLP)

```bash
# Enable distributed tracing
vertex node --tracing --tracing.endpoint http://localhost:4317

# Adjust sampling (default: 1.0 = 100%)
vertex node --tracing --tracing.sampling-ratio 0.1
```

### Logging

```bash
# JSON format for log aggregation
vertex node --log.format json

# Debug level
vertex node --log.level debug

# File output with rotation
vertex node --log.file /var/log/vertex/vertex.log
```

## Key Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `topology_connected_peers` | Gauge | Current connected peer count by type |
| `topology_depth` | Gauge | Current Kademlia depth |
| `handshake_duration_seconds` | Histogram | Handshake latency distribution |
| `topology_connections_total` | Counter | Total connection attempts |
| `executor.tasks.running` | Gauge | Active background tasks |

## Trace Endpoints

| Span | Description |
|------|-------------|
| `handshake.inbound` | Inbound peer handshake |
| `handshake.outbound` | Outbound peer handshake |
| `hive.exchange` | Peer discovery exchange |
| `pushsync.push` | Chunk upload operation |
| `retrieval.fetch` | Chunk retrieval operation |

## See Also

- [Architecture Overview](../architecture/overview.md) - Crate structure including observability crate
- [CLI Configuration](../cli/configuration.md) - Full CLI reference
