# Vertex Observability Stack

Local observability stack for Vertex Swarm development, providing:

- **Prometheus** - Metrics collection and storage
- **Tempo** - Distributed tracing (OTLP receiver)
- **Loki** - Log aggregation
- **Grafana** - Unified visualization

## Quick Start

```bash
# Start the stack
docker compose up -d

# Check status
docker compose ps

# View logs
docker compose logs -f
```

## Endpoints

| Service    | URL                          | Description                    |
|------------|------------------------------|--------------------------------|
| Grafana    | http://localhost:3000        | Dashboards (admin/admin)       |
| Prometheus | http://localhost:9099        | Metrics queries (host:9099→container:9090) |
| Tempo      | http://localhost:3200        | Trace queries                  |
| Loki       | http://localhost:3100        | Log queries                    |
| OTLP gRPC  | localhost:4317               | Trace ingestion (gRPC)         |
| OTLP HTTP  | http://localhost:4318        | Trace ingestion (HTTP)         |

## Configuring Vertex

### Metrics

Vertex exposes Prometheus metrics on `127.0.0.1:1637` by default. The metrics endpoint is opt-in: pass `--metrics` to enable it. The address and port are configurable with `--metrics.addr` and `--metrics.port`, and the metric-name prefix with `--metrics.prefix` (default `vertex`).

All services in this stack run with `network_mode: host`, so Prometheus reaches the node directly at `localhost:1637`; `prometheus/prometheus.yml` is pre-configured to scrape that target. There is no `host.docker.internal` indirection.

```bash
# Run vertex with the metrics endpoint on the default localhost:1637
vertex node --metrics
```

> Security: the metrics server is unauthenticated and, alongside `/metrics` and `/health`, mounts profiling and debug routes (`/debug/pprof/profile`, `/debug/memory`, `/debug/heap/dump`). Anyone who can reach the port can trigger CPU profiles, read allocator stats, and dump a heap profile. Keep the bind on `127.0.0.1` (the default) or behind a firewall, and never expose it to an untrusted network. For this local rig, the host-network Prometheus only needs `localhost`, so leave the default localhost bind in place rather than binding `0.0.0.0`.

### Tracing (OTLP)

Configure vertex to send traces to Tempo via OTLP. Tracing is opt-in: pass `--tracing` to enable it.

```bash
# Run vertex with OTLP tracing to Tempo
vertex node --tracing \
  --tracing.endpoint http://localhost:4317 \
  --tracing.service-name vertex-local
```

Or programmatically:

```rust
use vertex_observability::{OtlpConfig, VertexTracer};

let _guard = VertexTracer::new()
    .with_otlp(OtlpConfig::new(
        "http://localhost:4317",  // Tempo OTLP endpoint
        "vertex-local",           // Service name
        1.0,                      // Sampling ratio (1.0 = 100%)
    ))
    .init()?;
```

### Logs

The stack runs Promtail unconditionally (it is a plain service in `docker-compose.yml`, not gated behind a compose profile). To ship logs to Loki:

1. JSON logs to stdout: run vertex with `--log.json` and point Promtail at the container/stdout stream it tails.
2. File logs: write logs to a path Promtail watches and mount it into the Promtail container.

OTLP log export straight to Loki is also available from the node with `--tracing.logs` (endpoint via `--tracing.logs-endpoint`), bypassing Promtail entirely.

## Grafana Dashboards

Pre-provisioned dashboards:

- **Vertex Overview** - Topology, connections, protocols, and resources

Access Grafana at http://localhost:3000 (login: admin/admin)

### Creating Custom Dashboards

1. Create in Grafana UI
2. Export JSON
3. Save to `grafana/provisioning/dashboards/json/`
4. Restart Grafana: `docker compose restart grafana`

## Exploring Traces

1. Open Grafana → Explore
2. Select "Tempo" datasource
3. Search by:
   - Service name: `vertex-local`
   - Span name: e.g., `handshake`, `protocol`, `db_get`
   - Duration: e.g., `> 100ms`
   - Tags: e.g., `peer_id`, `direction`

### Trace to Logs Correlation

Grafana is configured to link traces to logs via `trace_id`. Ensure your logs include:

```json
{"trace_id": "abc123...", "message": "..."}
```

## Service Graph

Tempo generates service graphs from traces. View in Grafana:

1. Explore → Tempo
2. Click "Service Graph" tab
3. See communication patterns between components

## Alerting (Optional)

To enable alerting:

1. Add alert rules to `prometheus/alerts/`
2. Configure Alertmanager in `docker-compose.yml`
3. Uncomment alerting sections in `prometheus/prometheus.yml`

## Cleanup

```bash
# Stop services
docker compose down

# Remove volumes (delete all data)
docker compose down -v
```

## Troubleshooting

### No metrics in Prometheus

1. Check vertex is exposing metrics: `curl http://localhost:1637/metrics` (and confirm you started vertex with `--metrics`)
2. Verify the scrape target: Check Prometheus targets at http://localhost:9099/targets
3. Because the stack uses host networking, the node only needs to listen on `localhost:1637` (the default). No `0.0.0.0` bind is required, and you should not add one.

### No traces in Tempo

1. Verify OTLP endpoint is reachable: `curl http://localhost:4318/v1/traces`
2. Check Tempo logs: `docker compose logs tempo`
3. Ensure sampling ratio > 0

### Grafana shows no data

1. Check datasource connectivity in Grafana → Settings → Data Sources
2. Verify time range is appropriate
3. Check metric names match vertex's actual metric names

## Architecture

```
┌─────────────┐     metrics      ┌─────────────┐
│   Vertex    │ ───────────────► │ Prometheus  │
│   (1637)    │                  │  (9099)     │
└─────────────┘                  └──────┬──────┘
      │                                 │
      │ OTLP traces                     │ metrics
      ▼                                 │
┌─────────────┐     metrics      ┌──────▼──────┐
│   Tempo     │ ───────────────► │  Grafana    │
│  (4317)     │                  │  (3000)     │
└─────────────┘                  └──────┬──────┘
                                       │
┌─────────────┐     logs               │ logs
│   Loki      │ ◄──────────────────────┘
│  (3100)     │
└─────────────┘
      ▲
      │ logs
┌─────────────┐
│  Promtail   │
└─────────────┘
```
