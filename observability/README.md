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
| Prometheus | http://localhost:9099        | Metrics queries                |
| Tempo      | http://localhost:3200        | Trace queries                  |
| Loki       | http://localhost:3100        | Log queries                    |
| OTLP gRPC  | localhost:4317               | Trace ingestion (gRPC)         |
| OTLP HTTP  | http://localhost:4318        | Trace ingestion (HTTP)         |

## Configuring Vertex

### Metrics

Vertex exposes Prometheus metrics on a configurable port (default: 9191).
The stack is pre-configured to scrape `host.docker.internal:9191`.

```bash
# Run vertex with metrics enabled
vertex node --metrics-addr 0.0.0.0:9191
```

### Tracing (OTLP)

Configure vertex to send traces to Tempo via OTLP:

```bash
# Run vertex with OTLP tracing
vertex node --otlp-endpoint http://localhost:4317 --otlp-service-name vertex-local
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

For log shipping to Loki, you have several options:

1. **JSON logs to stdout** - Configure vertex to output JSON logs, then use promtail:
   ```bash
   docker compose --profile logs up -d
   ```

2. **Direct Loki push** - Add a Loki tracing layer (future enhancement)

3. **File logs** - Write to `/var/log/vertex/` and mount in promtail

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
   - Span name: e.g., `handshake`, `hive`, `pushsync`
   - Duration: e.g., `> 100ms`
   - Tags: e.g., `peer_id`, `overlay`

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

1. Check vertex is exposing metrics: `curl http://localhost:9191/metrics`
2. Verify Prometheus can reach host: Check Prometheus targets page
3. On Linux, you may need `network_mode: host` or proper host networking

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
│   (9191)    │                  │  (9090)     │
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
│  Promtail   │ (optional)
└─────────────┘
```
