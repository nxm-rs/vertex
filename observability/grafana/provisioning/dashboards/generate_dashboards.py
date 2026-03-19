#!/usr/bin/env python3
"""Generate Grafana dashboards for Vertex observability stack.

Produces 5 dashboard JSON files:
  - vertex-overview.json   : High-level KPIs and health summary
  - vertex-topology.json   : Kademlia routing, connections, dialing, gossip
  - vertex-protocols.json  : Protocol streams, handshake, hive, pingpong
  - vertex-peers.json      : Peer management, scoring, health, registry
  - vertex-system.json     : Task executor, memory, process metrics

Usage:
    python3 generate_dashboards.py
"""

import json
from pathlib import Path

OUTPUT_DIR = Path(__file__).parent / "json"

# ---------------------------------------------------------------------------
# Panel ID counter
# ---------------------------------------------------------------------------
_panel_id = 0


def _next_id():
    global _panel_id
    _panel_id += 1
    return _panel_id


def _reset_ids():
    global _panel_id
    _panel_id = 0


# ---------------------------------------------------------------------------
# Shared template variables
# ---------------------------------------------------------------------------
TEMPLATE_VARS = [
    {
        "current": {"selected": True, "text": "Prometheus", "value": "prometheus"},
        "hide": 0,
        "includeAll": False,
        "label": "Data Source",
        "multi": False,
        "name": "datasource",
        "options": [],
        "query": "prometheus",
        "refresh": 1,
        "regex": "",
        "skipUrlSync": False,
        "type": "datasource",
    },
    {
        "current": {"selected": False, "text": "All", "value": "$__all"},
        "datasource": {"type": "prometheus", "uid": "${datasource}"},
        "definition": 'label_values(up{job="vertex"}, instance)',
        "hide": 0,
        "includeAll": True,
        "label": "Instance",
        "multi": True,
        "name": "instance",
        "options": [],
        "query": {
            "query": 'label_values(up{job="vertex"}, instance)',
            "refId": "PrometheusVariableQueryEditor-VariableQuery",
        },
        "refresh": 2,
        "regex": "",
        "skipUrlSync": False,
        "sort": 1,
        "type": "query",
    },
    {
        "current": {"selected": True, "text": "5m", "value": "5m"},
        "hide": 0,
        "includeAll": False,
        "label": "Rate Window",
        "multi": False,
        "name": "rate_window",
        "options": [
            {"selected": False, "text": "1m", "value": "1m"},
            {"selected": True, "text": "5m", "value": "5m"},
            {"selected": False, "text": "10m", "value": "10m"},
            {"selected": False, "text": "15m", "value": "15m"},
            {"selected": False, "text": "30m", "value": "30m"},
            {"selected": False, "text": "1h", "value": "1h"},
        ],
        "query": "1m,5m,10m,15m,30m,1h",
        "skipUrlSync": False,
        "type": "custom",
    },
]

# ---------------------------------------------------------------------------
# PromQL helpers
# ---------------------------------------------------------------------------
I = '{instance=~"$instance"}'
# Purpose-scoped instance filters for multi-swarm metrics.
TOPO = '{purpose="topology", instance=~"$instance"}'
VRFY = '{purpose="verifier", instance=~"$instance"}'


def g(metric, labels=""):
    """Gauge query."""
    if labels:
        return f'{metric}{{{labels}, instance=~"$instance"}}'
    return f"{metric}{I}"


def r(metric, labels="", window="$rate_window"):
    """Rate query."""
    if labels:
        return f'rate({metric}{{{labels}, instance=~"$instance"}}[{window}])'
    return f"rate({metric}{I}[{window}])"


def sr(metric, by="", labels="", window="$rate_window"):
    """Sum of rate, optionally grouped."""
    base = r(metric, labels, window)
    if by:
        return f"sum by ({by}) ({base})"
    return f"sum({base})"


def ri(metric, labels="", window="$__rate_interval"):
    """Rate with __rate_interval."""
    if labels:
        return f'rate({metric}{{{labels}, instance=~"$instance"}}[{window}])'
    return f"rate({metric}{I}[{window}])"


def sri(metric, by="", labels="", window="$__rate_interval"):
    """Sum of rate with __rate_interval."""
    base = ri(metric, labels, window)
    if by:
        return f"sum by ({by}) ({base})"
    return f"sum({base})"


def hq(q, metric, by="", labels="", window="$rate_window"):
    """Histogram quantile."""
    if labels:
        bkt = f'{metric}_bucket{{{labels}, instance=~"$instance"}}'
    else:
        bkt = f"{metric}_bucket{I}"
    inner = f"rate({bkt}[{window}])"
    if by:
        inner = f"sum by (le, {by}) ({inner})"
    else:
        inner = f"sum by (le) ({inner})"
    return f"histogram_quantile({q}, {inner})"


def inc(metric, labels="", window="1h"):
    """Increase over window."""
    if labels:
        return f'increase({metric}{{{labels}, instance=~"$instance"}}[{window}])'
    return f"increase({metric}{I}[{window}])"


def sinc(metric, labels="", window="1h"):
    """Sum of increase."""
    return f"sum({inc(metric, labels, window)}) or vector(0)"


# ---------------------------------------------------------------------------
# Panel builders
# ---------------------------------------------------------------------------
def _ds():
    return {"type": "prometheus", "uid": "${datasource}"}


def _tgt(expr, legend=""):
    return {"datasource": _ds(), "expr": expr, "legendFormat": legend}


REFS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ"


def _assign_refs(targets):
    """Assign refId to each target."""
    tgts = targets if isinstance(targets, list) else [targets]
    for i, t in enumerate(tgts):
        t["refId"] = REFS[i] if i < len(REFS) else f"ref{i}"
    return tgts


def row_panel(title, y, collapsed=False, panels=None):
    r = {
        "gridPos": {"h": 1, "w": 24, "x": 0, "y": y},
        "id": _next_id(),
        "title": title,
        "type": "row",
        "collapsed": collapsed,
    }
    if collapsed and panels:
        r["panels"] = panels
    return r


def stat(title, targets, x, y, w=4, h=4, unit="short", thresholds=None, decimals=None):
    if thresholds is None:
        thresholds = {
            "mode": "absolute",
            "steps": [{"color": "green", "value": None}],
        }
    defaults = {
        "color": {"mode": "thresholds"},
        "mappings": [],
        "thresholds": thresholds,
        "unit": unit,
    }
    if decimals is not None:
        defaults["decimals"] = decimals
    return {
        "datasource": _ds(),
        "fieldConfig": {"defaults": defaults, "overrides": []},
        "gridPos": {"h": h, "w": w, "x": x, "y": y},
        "id": _next_id(),
        "options": {
            "colorMode": "value",
            "graphMode": "area",
            "justifyMode": "auto",
            "orientation": "auto",
            "reduceOptions": {
                "calcs": ["lastNotNull"],
                "fields": "",
                "values": False,
            },
            "textMode": "auto",
        },
        "targets": _assign_refs(targets if isinstance(targets, list) else [targets]),
        "title": title,
        "type": "stat",
    }


def ts(
    title,
    targets,
    x,
    y,
    w=12,
    h=8,
    unit="short",
    fill=0,
    stack=False,
    legend="list",
    decimals=None,
    min_val=None,
    max_val=None,
    draw="line",
    tooltip="single",
):
    defaults = {
        "color": {"mode": "palette-classic"},
        "custom": {
            "axisBorderShow": False,
            "axisCenteredZero": False,
            "axisColorMode": "text",
            "axisLabel": "",
            "axisPlacement": "auto",
            "barAlignment": 0,
            "drawStyle": draw,
            "fillOpacity": 30 if stack else fill,
            "gradientMode": "none",
            "hideFrom": {"legend": False, "tooltip": False, "viz": False},
            "insertNulls": False,
            "lineInterpolation": "linear",
            "lineWidth": 1,
            "pointSize": 5,
            "scaleDistribution": {"type": "linear"},
            "showPoints": "auto",
            "spanNulls": False,
            "stacking": {
                "group": "A",
                "mode": "normal" if stack else "none",
            },
            "thresholdsStyle": {"mode": "off"},
        },
        "mappings": [],
        "thresholds": {
            "mode": "absolute",
            "steps": [{"color": "green", "value": None}],
        },
        "unit": unit,
    }
    if decimals is not None:
        defaults["decimals"] = decimals
    if min_val is not None:
        defaults["min"] = min_val
    if max_val is not None:
        defaults["max"] = max_val
    return {
        "datasource": _ds(),
        "fieldConfig": {"defaults": defaults, "overrides": []},
        "gridPos": {"h": h, "w": w, "x": x, "y": y},
        "id": _next_id(),
        "options": {
            "legend": {"calcs": [], "displayMode": legend, "placement": "bottom"},
            "tooltip": {"mode": tooltip, "sort": "none"},
        },
        "targets": _assign_refs(
            targets if isinstance(targets, list) else [targets]
        ),
        "title": title,
        "type": "timeseries",
    }


def heatmap(title, expr, x, y, w=12, h=8, unit="s", target_format="heatmap",
            rows_layout="auto"):
    return {
        "datasource": _ds(),
        "fieldConfig": {"defaults": {}, "overrides": []},
        "gridPos": {"h": h, "w": w, "x": x, "y": y},
        "id": _next_id(),
        "options": {
            "calculate": False,
            "cellGap": 1,
            "color": {
                "exponent": 0.5,
                "fill": "dark-orange",
                "mode": "scheme",
                "reverse": False,
                "scale": "exponential",
                "scheme": "Oranges",
                "steps": 64,
            },
            "exemplars": {"color": "rgba(255,0,255,0.7)"},
            "filterValues": {"le": 1e-9},
            "legend": {"show": True},
            "rowsFrame": {"layout": rows_layout},
            "tooltip": {"show": True, "yHistogram": False},
            "yAxis": {"axisPlacement": "left", "reverse": False, "unit": unit},
        },
        "targets": _assign_refs([{**_tgt(expr), "format": target_format}]),
        "title": title,
        "type": "heatmap",
    }


def table(title, targets, x, y, w=24, h=8, overrides=None):
    tgts = _assign_refs(targets if isinstance(targets, list) else [targets])
    for t in tgts:
        t["instant"] = True
        t["format"] = "table"
    p = {
        "datasource": _ds(),
        "fieldConfig": {
            "defaults": {"custom": {"align": "auto"}},
            "overrides": overrides or [],
        },
        "gridPos": {"h": h, "w": w, "x": x, "y": y},
        "id": _next_id(),
        "options": {"showHeader": True, "footer": {"show": False}},
        "targets": tgts,
        "title": title,
        "type": "table",
    }
    return p


def bargauge(title, targets, x, y, w=12, h=8, unit="short", orientation="horizontal"):
    return {
        "datasource": _ds(),
        "fieldConfig": {
            "defaults": {
                "color": {"mode": "palette-classic"},
                "mappings": [],
                "thresholds": {
                    "mode": "absolute",
                    "steps": [{"color": "green", "value": None}],
                },
                "unit": unit,
            },
            "overrides": [],
        },
        "gridPos": {"h": h, "w": w, "x": x, "y": y},
        "id": _next_id(),
        "options": {
            "displayMode": "gradient",
            "minVizHeight": 10,
            "minVizWidth": 0,
            "orientation": orientation,
            "reduceOptions": {
                "calcs": ["lastNotNull"],
                "fields": "",
                "values": False,
            },
            "showUnfilled": True,
        },
        "targets": _assign_refs(
            targets if isinstance(targets, list) else [targets]
        ),
        "title": title,
        "type": "bargauge",
    }


# ---------------------------------------------------------------------------
# Dashboard wrapper
# ---------------------------------------------------------------------------
def dashboard(uid, title, description, tags, panels, links=None):
    return {
        "annotations": {
            "list": [
                {
                    "builtIn": 1,
                    "datasource": {"type": "grafana", "uid": "-- Grafana --"},
                    "enable": True,
                    "hide": True,
                    "iconColor": "rgba(0, 211, 255, 1)",
                    "name": "Annotations & Alerts",
                    "type": "dashboard",
                }
            ]
        },
        "description": description,
        "editable": True,
        "fiscalYearStartMonth": 0,
        "graphTooltip": 1,
        "links": links or [],
        "panels": panels,
        "schemaVersion": 39,
        "tags": tags,
        "templating": {"list": TEMPLATE_VARS},
        "time": {"from": "now-1h", "to": "now"},
        "timepicker": {},
        "timezone": "browser",
        "title": title,
        "uid": uid,
        "version": 1,
        "refresh": "10s",
    }


def dash_link(title, uid):
    return {
        "asDropdown": False,
        "icon": "external link",
        "includeVars": True,
        "keepTime": True,
        "tags": [],
        "targetBlank": False,
        "title": title,
        "tooltip": "",
        "type": "link",
        "url": f"/d/{uid}",
    }


# ---------------------------------------------------------------------------
# 1. OVERVIEW DASHBOARD
# ---------------------------------------------------------------------------
def build_overview():
    _reset_ids()
    y = 0
    panels = []

    links = [
        dash_link("Topology", "vertex-topology"),
        dash_link("Protocols", "vertex-protocols"),
        dash_link("Peers", "vertex-peers"),
        dash_link("Database", "vertex-database"),
        dash_link("System", "vertex-system"),
    ]

    # -- KPI row
    panels.append(row_panel("Key Metrics", y))
    y += 1
    panels.append(
        stat("Connected Peers", [_tgt(f'sum(vertex_topology_connected_peers{I})', "Connected")], 0, y)
    )
    panels.append(
        stat("Kademlia Depth", [_tgt(g("vertex_topology_depth"), "Depth")], 4, y)
    )
    panels.append(
        stat("Indexed Peers", [_tgt(g("vertex_peer_manager_total_peers"), "Indexed")], 8, y)
    )
    panels.append(
        stat(
            "Pending",
            [_tgt(g("vertex_peer_registry_pending_connections"), "Pending")],
            12,
            y,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 10},
                    {"color": "red", "value": 50},
                ],
            },
        )
    )
    panels.append(
        stat(
            "Failing",
            [_tgt(g("vertex_peer_manager_health", 'state="failing"'), "Failing")],
            16,
            y,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 10},
                    {"color": "red", "value": 50},
                ],
            },
        )
    )
    panels.append(
        stat(
            "Banned",
            [_tgt(g("vertex_peer_manager_health", 'state="banned"'), "Banned")],
            20,
            y,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 1},
                    {"color": "red", "value": 5},
                ],
            },
        )
    )
    y += 4

    # -- Network Health
    panels.append(row_panel("Network Health", y))
    y += 1
    panels.append(
        ts(
            "Connected Peers",
            [
                _tgt(f'sum(vertex_topology_connected_peers{I})', "Total"),
                _tgt(g("vertex_topology_connected_peers", 'node_type="storer"'), "Storers"),
                _tgt(g("vertex_topology_connected_peers", 'node_type="client"'), "Clients"),
            ],
            0, y, w=8,
        )
    )
    panels.append(
        ts(
            "Connection Events /s",
            [
                _tgt(sr("vertex_topology_connections_total"), "Connections"),
                _tgt(sr("vertex_topology_disconnections_total"), "Disconnections"),
                _tgt(sr("vertex_topology_connections_rejected_total"), "Rejected"),
            ],
            8, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Kademlia Depth",
            [
                _tgt(g("vertex_topology_depth"), "Depth"),
            ],
            16, y, w=8, fill=10,
        )
    )
    y += 8

    # -- Protocol Health
    panels.append(row_panel("Protocol Health", y))
    y += 1
    panels.append(
        ts(
            "Protocol Exchange Rate /s",
            [_tgt(sri("vertex_protocol_exchanges_total", by="protocol, direction"), "{{protocol}} {{direction}}")],
            0, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Protocol Exchange Outcomes /s",
            [_tgt(sri("vertex_protocol_exchange_outcomes_total", by="protocol, outcome"), "{{protocol}} {{outcome}}")],
            8, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Protocol Upgrade Errors /s",
            [_tgt(sri("vertex_protocol_upgrade_errors_total", by="protocol, reason"), "{{protocol}} {{reason}}")],
            16, y, w=8, unit="ops",
        )
    )
    y += 8

    # -- Resource Summary
    panels.append(row_panel("Resources", y))
    y += 1
    panels.append(
        ts(
            "Memory Usage",
            [
                _tgt(g("vertex_jemalloc_allocated_bytes"), "Allocated"),
                _tgt(g("vertex_jemalloc_resident_bytes"), "Resident"),
                _tgt(g("vertex_process_resident_memory_bytes"), "Process RSS"),
            ],
            0, y, w=8, unit="bytes",
        )
    )
    panels.append(
        ts(
            "CPU Usage",
            [_tgt(r("vertex_process_cpu_seconds_total"), "CPU")],
            8, y, w=8, unit="percentunit",
        )
    )
    panels.append(
        ts(
            "File Descriptors",
            [
                _tgt(g("vertex_process_open_fds"), "Open"),
                _tgt(g("vertex_process_max_fds"), "Max"),
            ],
            16, y, w=8,
        )
    )
    y += 8

    # -- Storage Summary
    panels.append(row_panel("Storage", y))
    y += 1
    panels.append(
        stat("DB File Size", [_tgt(g("vertex_redb_file_size_bytes"), "")], 0, y, unit="bytes")
    )
    panels.append(
        stat(
            "Total Entries",
            [_tgt(f'sum(vertex_db_entries{I})', "Entries")],
            4, y,
        )
    )
    panels.append(
        stat(
            "Fragmentation",
            [
                _tgt(
                    f'vertex_redb_fragmented_bytes_total{I} / '
                    f'(vertex_redb_stored_bytes_total{I} + vertex_redb_metadata_bytes_total{I} + vertex_redb_fragmented_bytes_total{I})',
                    "Ratio",
                )
            ],
            8, y,
            unit="percentunit",
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 0.2},
                    {"color": "red", "value": 0.5},
                ],
            },
        )
    )
    panels.append(
        stat(
            "DB Ops /s",
            [_tgt(sr("vertex_db_operations_total"), "Ops/s")],
            12, y,
            unit="ops",
        )
    )

    return dashboard(
        "vertex-overview",
        "Vertex Overview",
        "High-level health and KPIs for the Vertex Swarm node",
        ["vertex", "overview"],
        panels,
        links,
    )


# ---------------------------------------------------------------------------
# 2. TOPOLOGY DASHBOARD
# ---------------------------------------------------------------------------
def build_topology():
    _reset_ids()
    y = 0
    panels = []

    links = [
        dash_link("Overview", "vertex-overview"),
        dash_link("Protocols", "vertex-protocols"),
        dash_link("Peers", "vertex-peers"),
        dash_link("Database", "vertex-database"),
        dash_link("System", "vertex-system"),
    ]

    # -- Kademlia State
    panels.append(row_panel("Kademlia State", y))
    y += 1
    panels.append(stat("Depth", [_tgt(g("vertex_topology_depth"), "Depth")], 0, y, w=3))
    panels.append(stat("Connected", [_tgt(f'sum(vertex_topology_connected_peers{I})', "Connected")], 3, y, w=3))
    panels.append(stat("Indexed", [_tgt(g("vertex_peer_manager_total_peers"), "Indexed")], 6, y, w=3))
    panels.append(
        stat(
            "Banned",
            [_tgt(g("vertex_peer_manager_health", 'state="banned"'), "Banned")],
            9, y, w=3,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 1},
                    {"color": "red", "value": 5},
                ],
            },
        )
    )
    panels.append(
        stat(
            "Depth Increases (1h)",
            [_tgt(sinc("vertex_topology_depth_increases_total"), "Increases")],
            12, y, w=4,
        )
    )
    panels.append(
        stat(
            "Depth Decreases (1h)",
            [_tgt(sinc("vertex_topology_depth_decreases_total"), "Decreases")],
            16, y, w=4,
        )
    )
    panels.append(
        stat(
            "Dial Exhausted (1h)",
            [_tgt(sinc("vertex_topology_dial_exhausted_total"), "Exhausted")],
            20, y, w=4,
        )
    )
    y += 4

    panels.append(
        ts(
            "Connected Peers by Type",
            [
                _tgt(f'sum(vertex_topology_connected_peers{I})', "Total"),
                _tgt(g("vertex_topology_connected_peers", 'node_type="storer"'), "Storers"),
                _tgt(g("vertex_topology_connected_peers", 'node_type="client"'), "Clients"),
            ],
            0, y, w=12,
        )
    )
    panels.append(
        ts(
            "Depth Over Time",
            [
                _tgt(g("vertex_topology_depth"), "Depth"),
                _tgt(sr("vertex_topology_depth_increases_total"), "Increases /s"),
                _tgt(sr("vertex_topology_depth_decreases_total"), "Decreases /s"),
            ],
            12, y, w=12,
        )
    )
    y += 8

    # -- Bin Analysis
    panels.append(row_panel("Bin Analysis", y))
    y += 1
    panels.append(
        ts(
            "Connected Peers per Bin",
            [_tgt(g("vertex_topology_bin_connected_peers"), "bin {{bin}}")],
            0, y, w=12, stack=True,
        )
    )
    panels.append(
        ts(
            "Known Peers per Bin",
            [_tgt(g("vertex_topology_bin_known_peers"), "bin {{bin}}")],
            12, y, w=12, stack=True,
        )
    )
    y += 8
    panels.append(
        ts(
            "Target vs Connected vs Active per Bin",
            [
                _tgt(g("vertex_topology_bin_connected_peers"), "connected bin {{bin}}"),
                _tgt(g("vertex_topology_bin_target_peers"), "target bin {{bin}}"),
                _tgt(g("vertex_topology_bin_active"), "active bin {{bin}}"),
            ],
            0, y, w=12,
        )
    )
    panels.append(
        ts(
            "Bin Limits: Effective / Nominal / Ceiling",
            [
                _tgt(g("vertex_topology_bin_effective"), "effective bin {{bin}}"),
                _tgt(g("vertex_topology_bin_nominal_peers"), "nominal"),
                _tgt(g("vertex_topology_bin_ceiling_peers"), "ceiling bin {{bin}}"),
            ],
            12, y, w=12,
        )
    )
    y += 8
    panels.append(
        ts(
            "Bin States: Dialing / Handshaking",
            [
                _tgt(g("vertex_topology_bin_dialing"), "dialing bin {{bin}}"),
                _tgt(g("vertex_topology_bin_handshaking"), "handshaking bin {{bin}}"),
            ],
            0, y, w=12,
        )
    )
    panels.append(
        ts(
            "Bin Index Size",
            [_tgt(g("vertex_topology_bin_index_size"), "bin {{bin}}")],
            12, y, w=12,
        )
    )
    y += 8

    # -- Connection Lifecycle
    panels.append(row_panel("Connection Lifecycle", y))
    y += 1
    panels.append(
        ts(
            "Connections /s by Direction & Type",
            [_tgt(sri("vertex_topology_connections_total", by="node_type, direction, outcome"), "{{direction}} {{node_type}} {{outcome}}")],
            0, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Rejections /s by Reason",
            [_tgt(sri("vertex_topology_connections_rejected_total", by="reason, direction"), "{{reason}} ({{direction}})")],
            8, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Disconnections /s by Reason",
            [_tgt(sri("vertex_topology_disconnections_total", by="reason, connection_type"), "{{reason}} ({{connection_type}})")],
            16, y, w=8, unit="ops",
        )
    )
    y += 8
    panels.append(
        ts(
            "Connection Duration (p50 / p95 / p99)",
            [
                _tgt(hq(0.50, "vertex_topology_connection_duration_seconds", by="node_type"), "p50 {{node_type}}"),
                _tgt(hq(0.95, "vertex_topology_connection_duration_seconds", by="node_type"), "p95 {{node_type}}"),
                _tgt(hq(0.99, "vertex_topology_connection_duration_seconds", by="node_type"), "p99 {{node_type}}"),
            ],
            0, y, w=12, unit="s",
        )
    )
    panels.append(
        ts(
            "Phase Transitions /s",
            [_tgt(sri("vertex_topology_phase_transitions_total", by="from, to"), "{{from}} → {{to}}")],
            12, y, w=12, unit="ops",
        )
    )
    y += 8
    panels.append(
        ts(
            "Topology Handshake Duration (p50 / p99)",
            [
                _tgt(hq(0.50, "vertex_topology_handshake_duration_seconds"), "p50"),
                _tgt(hq(0.99, "vertex_topology_handshake_duration_seconds"), "p99"),
            ],
            0, y, w=12, unit="s",
        )
    )
    y += 8

    # -- Connection Stability
    panels.append(row_panel("Connection Stability", y))
    y += 1
    panels.append(
        ts(
            "Early Disconnects /s by Reason",
            [_tgt(sri("vertex_topology_early_disconnects_total", by="reason"), "{{reason}}")],
            0, y, w=8, unit="ops",
        )
    )
    panels.append(
        stat(
            "Early Disconnects (1h)",
            [_tgt(sinc("vertex_topology_early_disconnects_total"), "Total")],
            8, y, w=4,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 10},
                    {"color": "red", "value": 50},
                ],
            },
        )
    )
    panels.append(
        ts(
            "Dial Failures /s by Error Type",
            [_tgt(sri("vertex_topology_dial_failures_total", by="error_type"), "{{error_type}}")],
            12, y, w=12, unit="ops",
        )
    )
    y += 8

    # -- Dialer
    panels.append(row_panel("Dialer", y))
    y += 1
    panels.append(
        ts(
            "Dial Failures /s by Reason",
            [_tgt(sri("vertex_topology_dial_failures_total", by="reason"), "{{reason}}")],
            0, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Dial Duration (p50 / p95 / p99)",
            [
                _tgt(hq(0.50, "vertex_topology_dial_duration_seconds"), "p50"),
                _tgt(hq(0.95, "vertex_topology_dial_duration_seconds"), "p95"),
                _tgt(hq(0.99, "vertex_topology_dial_duration_seconds"), "p99"),
            ],
            8, y, w=8, unit="s",
        )
    )
    panels.append(
        ts(
            "Addresses per Dial Attempt (p50 / p95)",
            [
                _tgt(hq(0.50, "vertex_topology_dial_addr_count"), "p50"),
                _tgt(hq(0.95, "vertex_topology_dial_addr_count"), "p95"),
            ],
            16, y, w=8,
        )
    )
    y += 8
    panels.append(
        ts(
            "Dial Tracker Queue",
            [
                _tgt(g("vertex_dial_tracker_pending"), "{{purpose}} pending"),
                _tgt(g("vertex_dial_tracker_in_flight"), "{{purpose}} in-flight"),
            ],
            0, y, w=8,
        )
    )
    panels.append(
        ts(
            "Backoff & Ban State",
            [
                _tgt(g("vertex_dial_tracker_backoff_peers"), "{{purpose}} backoff"),
                _tgt(g("vertex_dial_tracker_banned_peers"), "{{purpose}} banned"),
            ],
            8, y, w=8,
        )
    )
    panels.append(
        ts(
            "Backoff & Ban Rate /s",
            [
                _tgt(sri("vertex_dial_tracker_backoff_recorded_total", by="purpose"), "{{purpose}} backoff"),
                _tgt(sri("vertex_dial_tracker_banned_total", by="purpose"), "{{purpose}} banned"),
            ],
            16, y, w=8, unit="ops",
        )
    )
    y += 8

    # -- Gossip Verifier
    panels.append(row_panel("Gossip Verifier", y))
    y += 1
    panels.append(
        ts(
            "Tracked Gossipers",
            [_tgt(g("vertex_topology_gossip_tracked_gossipers"), "Gossipers")],
            0, y, w=8,
        )
    )
    panels.append(
        ts(
            "Gossip Rejections /s by Reason",
            [_tgt(sri("vertex_topology_gossip_rejected_total", by="reason"), "{{reason}}")],
            8, y, w=8, unit="ops",
        )
    )
    y += 8

    # Verifier identify metrics
    panels.append(
        ts(
            "Verifier Identify Rate",
            [
                _tgt(sr("vertex_identify_received_total", labels='purpose="verifier"'), "Received"),
                _tgt(sr("vertex_identify_sent_total", labels='purpose="verifier"'), "Sent"),
                _tgt(sr("vertex_identify_error_total", labels='purpose="verifier"'), "Errors"),
            ],
            0, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Verifier Identify Duration (p50 / p99)",
            [
                _tgt(hq(0.50, "vertex_identify_duration_seconds", labels='purpose="verifier"'), "p50"),
                _tgt(hq(0.99, "vertex_identify_duration_seconds", labels='purpose="verifier"'), "p99"),
            ],
            8, y, w=8, unit="s",
        )
    )
    panels.append(
        ts(
            "Verifier Identify Errors /s by Kind",
            [_tgt(f'sum by (kind) (rate(vertex_identify_error_total{VRFY}[$__rate_interval]))', "{{kind}}")],
            16, y, w=8, unit="ops",
        )
    )
    y += 8

    # -- Proximity Cache
    panels.append(row_panel("Proximity Cache", y))
    y += 1
    panels.append(
        ts(
            "Cached Items",
            [_tgt(g("vertex_topology_proximity_cached_items"), "Items")],
            0, y, w=8,
        )
    )
    panels.append(
        ts(
            "Cache Generation",
            [_tgt(g("vertex_topology_proximity_generation"), "Generation")],
            8, y, w=8,
        )
    )
    panels.append(
        ts(
            "Index Mutation Rate",
            [_tgt(r("vertex_topology_proximity_generation"), "mutations/s")],
            16, y, w=8, unit="ops",
        )
    )
    y += 8

    # -- Performance
    panels.append(row_panel("Performance & Lock Contention", y))
    y += 1
    panels.append(
        ts(
            "Poll Loop Duration (p50 / p99)",
            [
                _tgt(hq(0.50, "vertex_topology_poll_duration_seconds"), "p50"),
                _tgt(hq(0.99, "vertex_topology_poll_duration_seconds"), "p99"),
            ],
            0, y, w=8, unit="s",
        )
    )
    panels.append(
        ts(
            "Poll Events /s",
            [_tgt(sr("vertex_topology_poll_events_total"), "Events")],
            8, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Lock Contention (p99)",
            [
                _tgt(hq(0.99, "vertex_topology_routing_phases_lock_seconds"), "Phases Lock p99"),
                _tgt(hq(0.99, "vertex_topology_routing_candidates_lock_seconds"), "Candidates Lock p99"),
            ],
            16, y, w=8, unit="s",
        )
    )
    y += 8

    return dashboard(
        "vertex-topology",
        "Vertex Topology",
        "Kademlia routing table, connections, dialing, gossip, and proximity cache",
        ["vertex", "topology"],
        panels,
        links,
    )


# ---------------------------------------------------------------------------
# 3. PROTOCOLS DASHBOARD
# ---------------------------------------------------------------------------
def build_protocols():
    _reset_ids()
    y = 0
    panels = []

    links = [
        dash_link("Overview", "vertex-overview"),
        dash_link("Topology", "vertex-topology"),
        dash_link("Peers", "vertex-peers"),
        dash_link("Database", "vertex-database"),
        dash_link("System", "vertex-system"),
    ]

    # -- Protocol Streams (libp2p layer)
    panels.append(row_panel("libp2p Protocol Streams", y))
    y += 1
    panels.append(
        ts(
            "Active Streams by Protocol",
            [_tgt(f'sum by (protocol, direction) (vertex_protocol_streams_active{I})', "{{protocol}} {{direction}}")],
            0, y, w=12,
        )
    )
    panels.append(
        ts(
            "Stream Throughput /s",
            [_tgt(f'sum by (protocol, direction) (rate(vertex_protocol_streams_total{I}[$__rate_interval]))', "{{protocol}} {{direction}}")],
            12, y, w=12, unit="ops",
        )
    )
    y += 8
    panels.append(
        ts(
            "Upgrade Errors /s by Protocol & Reason",
            [_tgt(sri("vertex_protocol_upgrade_errors_total", by="protocol, direction, reason"), "{{protocol}} {{direction}} {{reason}}")],
            0, y, w=24, unit="ops",
        )
    )
    y += 8

    # -- Protocol Exchanges (headers layer)
    panels.append(row_panel("Protocol Exchanges", y))
    y += 1
    panels.append(
        ts(
            "Exchange Rate /s by Protocol",
            [_tgt(sri("vertex_protocol_exchanges_total", by="protocol, direction"), "{{protocol}} {{direction}}")],
            0, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Exchange Outcomes /s",
            [_tgt(sri("vertex_protocol_exchange_outcomes_total", by="protocol, outcome"), "{{protocol}} {{outcome}}")],
            8, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Exchange Duration (avg) by Protocol",
            [
                _tgt(
                    f'sum by (protocol, direction) (rate(vertex_protocol_exchange_duration_seconds_sum{I}[$rate_window])) / '
                    f'clamp_min(sum by (protocol, direction) (rate(vertex_protocol_exchange_duration_seconds_count{I}[$rate_window])), 1)',
                    "{{protocol}} {{direction}}",
                )
            ],
            16, y, w=8, unit="s",
        )
    )
    y += 8
    panels.append(
        ts(
            "Exchange Duration p50 / p99 by Protocol",
            [
                _tgt(hq(0.50, "vertex_protocol_exchange_duration_seconds", by="protocol"), "p50 {{protocol}}"),
                _tgt(hq(0.99, "vertex_protocol_exchange_duration_seconds", by="protocol"), "p99 {{protocol}}"),
            ],
            0, y, w=24, unit="s",
        )
    )
    y += 8

    # -- Handshake Protocol
    panels.append(row_panel("Handshake Protocol", y))
    y += 1
    # Stats row
    panels.append(
        stat(
            "Active",
            [_tgt(f'sum(vertex_handshake_stage{TOPO}) or vector(0)', "Active")],
            0, y, w=4,
        )
    )
    panels.append(
        stat(
            "Success Rate (5m)",
            [
                _tgt(
                    f'sum(rate(vertex_handshake_success_total{TOPO}[5m])) / '
                    f'clamp_min(sum(rate(vertex_handshake_success_total{TOPO}[5m])) + '
                    f'sum(rate(vertex_handshake_failure_total{TOPO}[5m])), 1)',
                    "%",
                )
            ],
            4, y, w=4, unit="percentunit", decimals=1,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "red", "value": None},
                    {"color": "yellow", "value": 0.5},
                    {"color": "green", "value": 0.8},
                ],
            },
        )
    )
    panels.append(
        stat(
            "Avg Duration (5m)",
            [
                _tgt(
                    f'sum(rate(vertex_handshake_duration_seconds_sum{TOPO}[$rate_window])) / '
                    f'clamp_min(sum(rate(vertex_handshake_duration_seconds_count{TOPO}[$rate_window])), 1)',
                    "Avg",
                )
            ],
            8, y, w=4, unit="s", decimals=2,
        )
    )
    panels.append(
        stat("Success (1h)", [_tgt(sinc("vertex_handshake_success_total", labels='purpose="topology"'), "")], 12, y, w=3)
    )
    panels.append(
        stat(
            "Failed (1h)",
            [_tgt(sinc("vertex_handshake_failure_total", labels='purpose="topology"'), "")],
            15, y, w=3,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 1},
                    {"color": "red", "value": 10},
                ],
            },
        )
    )
    panels.append(
        stat("Attempts (1h)", [_tgt(sinc("vertex_handshake_total", labels='purpose="topology"'), "")], 18, y, w=3)
    )
    panels.append(
        stat("Total (1h)", [_tgt(sinc("vertex_handshake_total", labels='purpose="topology"'), "")], 21, y, w=3)
    )
    y += 4

    panels.append(
        ts(
            "Handshake Rate (Success/Failure)",
            [
                _tgt(sr("vertex_handshake_success_total", by="direction", labels='purpose="topology"'), "Success {{direction}}"),
                _tgt(sr("vertex_handshake_failure_total", by="direction", labels='purpose="topology"'), "Failure {{direction}}"),
            ],
            0, y, w=12, unit="ops",
        )
    )
    panels.append(
        ts(
            "Handshake Duration by Direction (p50 / p99)",
            [
                _tgt(hq(0.50, "vertex_handshake_duration_seconds", by="direction", labels='purpose="topology"'), "p50 {{direction}}"),
                _tgt(hq(0.99, "vertex_handshake_duration_seconds", by="direction", labels='purpose="topology"'), "p99 {{direction}}"),
            ],
            12, y, w=12, unit="s",
        )
    )
    y += 8

    panels.append(
        ts(
            "Handshake Stage Gauge",
            [_tgt(g("vertex_handshake_stage", 'purpose="topology"'), "{{direction}} {{stage}}")],
            0, y, w=12, stack=True,
        )
    )
    panels.append(
        ts(
            "Stage Duration Breakdown (avg)",
            [
                _tgt(
                    f'sum by (stage) (rate(vertex_handshake_stage_duration_seconds_sum{TOPO}[$rate_window])) / '
                    f'clamp_min(sum by (stage) (rate(vertex_handshake_stage_duration_seconds_count{TOPO}[$rate_window])), 1)',
                    "{{stage}}",
                )
            ],
            12, y, w=12, unit="s",
        )
    )
    y += 8

    panels.append(
        ts(
            "Failure Breakdown by Reason",
            [_tgt(sri("vertex_handshake_failure_total", by="reason, stage", labels='purpose="topology"'), "{{reason}} @ {{stage}}")],
            0, y, w=12, unit="ops",
        )
    )
    panels.append(
        heatmap(
            "Handshake Duration Heatmap",
            f'sum(increase(vertex_handshake_duration_seconds_bucket{TOPO}[$__rate_interval])) by (le)',
            12, y, w=12,
        )
    )
    y += 8

    # -- Hive Protocol
    panels.append(row_panel("Hive Protocol", y))
    y += 1
    panels.append(
        ts(
            "Peers per Exchange (p50 / p95)",
            [
                _tgt(hq(0.50, "vertex_hive_peers_per_exchange", by="direction"), "p50 {{direction}}"),
                _tgt(hq(0.95, "vertex_hive_peers_per_exchange", by="direction"), "p95 {{direction}}"),
            ],
            0, y, w=8,
        )
    )
    panels.append(
        ts(
            "Validation Failures /s by Reason",
            [_tgt(sri("vertex_hive_peer_validation_failures_total", by="reason"), "{{reason}}")],
            8, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Validation Duration (p50 / p99)",
            [
                _tgt(hq(0.50, "vertex_hive_validation_duration_seconds"), "p50"),
                _tgt(hq(0.99, "vertex_hive_validation_duration_seconds"), "p99"),
            ],
            16, y, w=8, unit="s",
        )
    )
    y += 8
    panels.append(
        ts(
            "Validation Cache Hit/Miss /s",
            [_tgt(sri("vertex_hive_validation_cache_total", by="outcome"), "{{outcome}}")],
            0, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Hive Rate Limited /s",
            [_tgt(sr("vertex_hive_rate_limited_total"), "Rate Limited")],
            8, y, w=8, unit="ops",
        )
    )
    panels.append(
        heatmap(
            "Validation Duration Heatmap",
            f'sum(increase(vertex_hive_validation_duration_seconds_bucket{I}[$__rate_interval])) by (le)',
            16, y, w=8,
        )
    )
    y += 8

    # -- Pingpong Protocol
    panels.append(row_panel("Pingpong Protocol", y))
    y += 1
    panels.append(
        ts(
            "RTT (p50 / p95 / p99)",
            [
                _tgt(hq(0.50, "vertex_pingpong_rtt_seconds"), "p50"),
                _tgt(hq(0.95, "vertex_pingpong_rtt_seconds"), "p95"),
                _tgt(hq(0.99, "vertex_pingpong_rtt_seconds"), "p99"),
            ],
            0, y, w=8, unit="s",
        )
    )
    panels.append(
        ts(
            "Exchange Outcomes /s",
            [_tgt(sri("vertex_pingpong_exchange_outcomes_total", by="direction, outcome"), "{{direction}} {{outcome}}")],
            8, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Errors /s by Reason",
            [_tgt(sri("vertex_pingpong_errors_total", by="direction, reason"), "{{direction}} {{reason}}")],
            16, y, w=8, unit="ops",
        )
    )
    y += 8
    panels.append(
        heatmap(
            "Pingpong RTT Heatmap",
            f'sum(increase(vertex_pingpong_rtt_seconds_bucket{I}[$__rate_interval])) by (le)',
            0, y, w=12,
        )
    )
    panels.append(
        ts(
            "Pingpong Exchanges /s",
            [_tgt(sri("vertex_pingpong_exchanges_total", by="direction"), "{{direction}}")],
            12, y, w=6, unit="ops",
        )
    )
    panels.append(
        ts(
            "Topology-level Ping RTT (p50 / p99)",
            [
                _tgt(hq(0.50, "vertex_topology_ping_rtt_seconds"), "p50"),
                _tgt(hq(0.99, "vertex_topology_ping_rtt_seconds"), "p99"),
                _tgt(sr("vertex_topology_pings_total"), "Pings /s"),
            ],
            18, y, w=6, unit="s",
        )
    )
    y += 8

    # -- Identify Protocol
    panels.append(row_panel("Identify Protocol", y))
    y += 1
    # Stats row (topology swarm only)
    panels.append(
        stat("Received (1h)", [_tgt(sinc("vertex_identify_received_total", labels='purpose="topology"'), "")], 0, y, w=4)
    )
    panels.append(
        stat("Sent (1h)", [_tgt(sinc("vertex_identify_sent_total", labels='purpose="topology"'), "")], 4, y, w=4)
    )
    panels.append(
        stat("Pushed (1h)", [_tgt(sinc("vertex_identify_pushed_total", labels='purpose="topology"'), "")], 8, y, w=4)
    )
    panels.append(
        stat(
            "Errors (1h)",
            [_tgt(sinc("vertex_identify_error_total", labels='purpose="topology"'), "")],
            12, y, w=4,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 1},
                    {"color": "red", "value": 10},
                ],
            },
        )
    )
    panels.append(
        stat(
            "Avg Duration (5m)",
            [
                _tgt(
                    f'sum(rate(vertex_identify_duration_seconds_sum{TOPO}[$rate_window])) / '
                    f'clamp_min(sum(rate(vertex_identify_duration_seconds_count{TOPO}[$rate_window])), 1)',
                    "Avg",
                )
            ],
            16, y, w=4, unit="s", decimals=2,
        )
    )
    y += 4

    panels.append(
        ts(
            "Identify Received by Agent Version",
            [_tgt(f'sum by (agent_version) (vertex_identify_received_total{TOPO})', "{{agent_version}}")],
            0, y, w=12, unit="short",
        )
    )
    panels.append(
        ts(
            "Identify Rate (Sent / Received / Errors)",
            [
                _tgt(sr("vertex_identify_received_total", labels='purpose="topology"'), "Received"),
                _tgt(sr("vertex_identify_sent_total", labels='purpose="topology"'), "Sent"),
                _tgt(sr("vertex_identify_pushed_total", labels='purpose="topology"'), "Pushed"),
                _tgt(sr("vertex_identify_error_total", labels='purpose="topology"'), "Errors"),
            ],
            12, y, w=12, unit="ops",
        )
    )
    y += 8

    panels.append(
        bargauge(
            "Agent Version Distribution",
            [_tgt(f'sort_desc(sum by (agent_version) (vertex_identify_received_total{TOPO}) > 0)', "{{agent_version}}")],
            0, y, w=12,
        )
    )
    panels.append(
        ts(
            "Identify Duration (p50 / p99)",
            [
                _tgt(hq(0.50, "vertex_identify_duration_seconds", labels='purpose="topology"'), "p50"),
                _tgt(hq(0.99, "vertex_identify_duration_seconds", labels='purpose="topology"'), "p99"),
            ],
            12, y, w=6, unit="s",
        )
    )
    panels.append(
        ts(
            "Errors /s by Kind",
            [_tgt(f'sum by (kind) (rate(vertex_identify_error_total{TOPO}[$__rate_interval]))', "{{kind}}")],
            18, y, w=6, unit="ops",
        )
    )
    y += 8

    panels.append(
        heatmap(
            "Identify Duration Heatmap",
            f'sum(increase(vertex_identify_duration_seconds_bucket{TOPO}[$__rate_interval])) by (le)',
            0, y, w=12,
        )
    )
    panels.append(
        table(
            "Top Agent Versions",
            [_tgt(f'sort_desc(sum by (agent_version) (vertex_identify_received_total{TOPO}) > 0)', "")],
            12, y, w=12,
        )
    )

    return dashboard(
        "vertex-protocols",
        "Vertex Protocols",
        "Protocol streams, exchanges, handshake, hive, pingpong, and identify details",
        ["vertex", "protocols"],
        panels,
        links,
    )


# ---------------------------------------------------------------------------
# 4. PEERS DASHBOARD
# ---------------------------------------------------------------------------
def build_peers():
    _reset_ids()
    y = 0
    panels = []

    links = [
        dash_link("Overview", "vertex-overview"),
        dash_link("Topology", "vertex-topology"),
        dash_link("Protocols", "vertex-protocols"),
        dash_link("Database", "vertex-database"),
        dash_link("System", "vertex-system"),
    ]

    # -- Peer Overview
    panels.append(row_panel("Peer Overview", y))
    y += 1
    panels.append(stat("Indexed Peers", [_tgt(g("vertex_peer_manager_total_peers"), "Indexed")], 0, y, w=4))
    panels.append(stat("Hot Peers", [_tgt(g("vertex_peer_manager_hot_peers"), "Hot")], 4, y, w=4))
    panels.append(stat("Stored Peers", [_tgt(g("vertex_peer_manager_stored_peers"), "Stored")], 8, y, w=4))
    panels.append(
        stat(
            "Stale",
            [_tgt(g("vertex_peer_manager_health", 'state="stale"'), "Stale")],
            12, y, w=3,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 5},
                    {"color": "red", "value": 20},
                ],
            },
        )
    )
    panels.append(
        stat(
            "Banned",
            [_tgt(g("vertex_peer_manager_health", 'state="banned"'), "Banned")],
            15, y, w=3,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 1},
                    {"color": "red", "value": 5},
                ],
            },
        )
    )
    panels.append(
        stat(
            "Failing",
            [_tgt(g("vertex_peer_manager_health", 'state="failing"'), "Failing")],
            18, y, w=3,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 10},
                    {"color": "red", "value": 50},
                ],
            },
        )
    )
    y += 4

    panels.append(
        ts(
            "Peer Manager State Over Time",
            [
                _tgt(g("vertex_peer_manager_total_peers"), "Indexed"),
                _tgt(g("vertex_peer_manager_hot_peers"), "Hot Cache"),
                _tgt(g("vertex_peer_manager_stored_peers"), "Stored (DB)"),
                _tgt(g("vertex_peer_manager_health", 'state="healthy"'), "Healthy"),
                _tgt(g("vertex_peer_manager_health", 'state="failing"'), "Failing"),
                _tgt(g("vertex_peer_manager_health", 'state="stale"'), "Stale"),
                _tgt(g("vertex_peer_manager_health", 'state="banned"'), "Banned"),
            ],
            0, y, w=12,
        )
    )
    panels.append(
        ts(
            "Connection Registry",
            [
                _tgt(g("vertex_peer_registry_active_connections"), "Active"),
                _tgt(g("vertex_peer_registry_pending_connections"), "Pending"),
            ],
            12, y, w=12,
        )
    )
    y += 8

    # -- Scoring
    panels.append(row_panel("Peer Scoring", y))
    y += 1
    _score = "vertex_peer_manager_score_distribution"
    panels.append(
        heatmap(
            "Score Distribution Heatmap",
            f'sum by (le) ({_score}{I})',
            0, y, w=24, unit="short",
            target_format="heatmap",
            rows_layout="le",
        )
    )
    y += 8

    # -- Health
    panels.append(row_panel("Peer Health", y))
    y += 1
    panels.append(
        ts(
            "Health State Breakdown",
            [
                _tgt(g("vertex_peer_manager_health", 'state="healthy"'), "Healthy"),
                _tgt(g("vertex_peer_manager_health", 'state="failing"'), "Failing"),
                _tgt(g("vertex_peer_manager_health", 'state="stale"'), "Stale"),
                _tgt(g("vertex_peer_manager_health", 'state="banned"'), "Banned"),
            ],
            0, y, w=24, stack=True,
        )
    )
    y += 8

    return dashboard(
        "vertex-peers",
        "Vertex Peers",
        "Peer management, scoring, health states, and connection registry",
        ["vertex", "peers"],
        panels,
        links,
    )


# ---------------------------------------------------------------------------
# 5. SYSTEM DASHBOARD
# ---------------------------------------------------------------------------
def build_system():
    _reset_ids()
    y = 0
    panels = []

    links = [
        dash_link("Overview", "vertex-overview"),
        dash_link("Topology", "vertex-topology"),
        dash_link("Protocols", "vertex-protocols"),
        dash_link("Peers", "vertex-peers"),
        dash_link("Database", "vertex-database"),
    ]

    # -- Task Executor
    panels.append(row_panel("Task Executor", y))
    y += 1
    panels.append(
        stat(
            "Critical Tasks",
            [_tgt(f'count(vertex_executor_tasks_running{I} > 0 and vertex_executor_tasks_running{{type="critical", instance=~"$instance"}}) or vector(0)', "")],
            0, y, w=4,
        )
    )
    panels.append(
        stat(
            "Regular Tasks",
            [_tgt(f'count(vertex_executor_tasks_running{I} > 0 and vertex_executor_tasks_running{{type="regular", instance=~"$instance"}}) or vector(0)', "")],
            4, y, w=4,
        )
    )
    panels.append(
        stat(
            "Blocking Tasks",
            [_tgt(f'count(vertex_executor_tasks_running{I} > 0 and vertex_executor_tasks_running{{type="blocking", instance=~"$instance"}}) or vector(0)', "")],
            8, y, w=4,
        )
    )
    panels.append(
        stat(
            "Panicked (Total)",
            [_tgt(f'sum(vertex_executor_tasks_panicked_total{I}) or vector(0)', "")],
            12, y, w=4,
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "red", "value": 1},
                ],
            },
        )
    )
    panels.append(
        stat(
            "Graceful Shutdown Pending",
            [_tgt(f'vertex_executor_tasks_graceful_shutdown_pending{I}', "")],
            16, y, w=4,
        )
    )
    panels.append(
        stat(
            "Graceful Shutdown Registered",
            [_tgt(f'vertex_executor_tasks_graceful_shutdown_registered{I}', "")],
            20, y, w=4,
        )
    )
    y += 4

    panels.append(
        ts(
            "Running Tasks by Type",
            [
                _tgt(f'count(vertex_executor_tasks_running{{type="critical", instance=~"$instance"}} > 0) or vector(0)', "Critical"),
                _tgt(f'count(vertex_executor_tasks_running{{type="regular", instance=~"$instance"}} > 0) or vector(0)', "Regular"),
                _tgt(f'count(vertex_executor_tasks_running{{type="blocking", instance=~"$instance"}} > 0) or vector(0)', "Blocking"),
            ],
            0, y, w=12, stack=True,
        )
    )
    panels.append(
        ts(
            "Task Spawn Rate /s",
            [
                _tgt(sri("vertex_executor_spawn_critical_tasks_total", by="task"), "Critical: {{task}}"),
                _tgt(sri("vertex_executor_spawn_regular_tasks_total", by="task"), "Regular: {{task}}"),
                _tgt(sri("vertex_executor_spawn_regular_blocking_tasks_total", by="task"), "Blocking: {{task}}"),
            ],
            12, y, w=12, unit="ops",
        )
    )
    y += 8

    panels.append(
        ts(
            "Task Completion Rate /s",
            [
                _tgt(sri("vertex_executor_spawn_finished_critical_tasks_total", by="task"), "Critical: {{task}}"),
                _tgt(sri("vertex_executor_spawn_finished_regular_tasks_total", by="task"), "Regular: {{task}}"),
                _tgt(sri("vertex_executor_spawn_finished_regular_blocking_tasks_total", by="task"), "Blocking: {{task}}"),
            ],
            0, y, w=12, unit="ops",
        )
    )
    panels.append(
        ts(
            "Graceful Shutdown State",
            [
                _tgt(g("vertex_executor_tasks_graceful_shutdown_registered"), "Registered"),
                _tgt(g("vertex_executor_tasks_graceful_shutdown_pending"), "Pending"),
                _tgt(sr("vertex_executor_tasks_graceful_shutdown_finished_total"), "Finished /s"),
            ],
            12, y, w=12,
        )
    )
    y += 8

    panels.append(
        table(
            "Running Tasks Detail",
            [_tgt(f'vertex_executor_tasks_running{I} > 0', "")],
            0, y, w=24,
        )
    )
    y += 8

    # -- Memory
    panels.append(row_panel("Memory", y))
    y += 1
    panels.append(
        ts(
            "jemalloc Memory",
            [
                _tgt(g("vertex_jemalloc_allocated_bytes"), "Allocated"),
                _tgt(g("vertex_jemalloc_active_bytes"), "Active"),
                _tgt(g("vertex_jemalloc_resident_bytes"), "Resident"),
                _tgt(g("vertex_jemalloc_mapped_bytes"), "Mapped"),
                _tgt(g("vertex_jemalloc_retained_bytes"), "Retained"),
            ],
            0, y, w=12, unit="bytes",
        )
    )
    panels.append(
        ts(
            "Process Memory",
            [
                _tgt(g("vertex_process_resident_memory_bytes"), "Resident (RSS)"),
                _tgt(g("vertex_process_virtual_memory_bytes"), "Virtual"),
                _tgt(g("vertex_process_virtual_memory_max_bytes"), "Virtual Max"),
            ],
            12, y, w=12, unit="bytes",
        )
    )
    y += 8

    panels.append(
        ts(
            "jemalloc Fragmentation (Resident - Allocated)",
            [
                _tgt(
                    f'vertex_jemalloc_resident_bytes{I} - vertex_jemalloc_allocated_bytes{I}',
                    "Fragmentation",
                )
            ],
            0, y, w=24, unit="bytes",
        )
    )
    y += 8

    # -- Process
    panels.append(row_panel("Process", y))
    y += 1
    panels.append(
        ts(
            "CPU Usage",
            [_tgt(r("vertex_process_cpu_seconds_total"), "CPU")],
            0, y, w=8, unit="percentunit",
        )
    )
    panels.append(
        ts(
            "File Descriptors",
            [
                _tgt(g("vertex_process_open_fds"), "Open"),
                _tgt(g("vertex_process_max_fds"), "Max"),
            ],
            8, y, w=8,
        )
    )
    panels.append(
        ts(
            "OS Threads",
            [_tgt(g("vertex_process_threads"), "Threads")],
            16, y, w=8,
        )
    )
    y += 8

    panels.append(
        stat(
            "Uptime",
            [
                _tgt(
                    f'time() - vertex_process_start_time_seconds{I}',
                    "Uptime",
                )
            ],
            0, y, w=6, unit="s",
        )
    )
    panels.append(
        stat(
            "Open FDs",
            [_tgt(g("vertex_process_open_fds"), "")],
            6, y, w=6,
        )
    )
    panels.append(
        stat(
            "OS Threads",
            [_tgt(g("vertex_process_threads"), "")],
            12, y, w=6,
        )
    )
    panels.append(
        stat(
            "Resident Memory",
            [_tgt(g("vertex_process_resident_memory_bytes"), "")],
            18, y, w=6, unit="bytes",
        )
    )

    return dashboard(
        "vertex-system",
        "Vertex System",
        "Task executor, memory allocator, and process-level metrics",
        ["vertex", "system"],
        panels,
        links,
    )


# ---------------------------------------------------------------------------
# 6. DATABASE DASHBOARD
# ---------------------------------------------------------------------------
def build_database():
    _reset_ids()
    y = 0
    panels = []

    links = [
        dash_link("Overview", "vertex-overview"),
        dash_link("Topology", "vertex-topology"),
        dash_link("Protocols", "vertex-protocols"),
        dash_link("Peers", "vertex-peers"),
        dash_link("System", "vertex-system"),
    ]

    # -- Database Overview (stat panels)
    panels.append(row_panel("Database Overview", y))
    y += 1
    panels.append(
        stat("File Size", [_tgt(g("vertex_redb_file_size_bytes"), "")], 0, y, unit="bytes")
    )
    panels.append(
        stat("Total Stored", [_tgt(g("vertex_redb_stored_bytes_total"), "")], 4, y, unit="bytes")
    )
    panels.append(
        stat(
            "Fragmentation",
            [
                _tgt(
                    f'vertex_redb_fragmented_bytes_total{I} / '
                    f'(vertex_redb_stored_bytes_total{I} + vertex_redb_metadata_bytes_total{I} + vertex_redb_fragmented_bytes_total{I})',
                    "Ratio",
                )
            ],
            8, y,
            unit="percentunit",
            thresholds={
                "mode": "absolute",
                "steps": [
                    {"color": "green", "value": None},
                    {"color": "yellow", "value": 0.2},
                    {"color": "red", "value": 0.5},
                ],
            },
        )
    )
    panels.append(
        stat("Total Entries", [_tgt(f'sum(vertex_db_entries{I})', "")], 12, y)
    )
    panels.append(
        stat(
            "Cache Evictions /s",
            [_tgt(r("vertex_redb_cache_evictions_total"), "")],
            16, y,
            unit="ops",
        )
    )
    panels.append(
        stat("Metadata", [_tgt(g("vertex_redb_metadata_bytes_total"), "")], 20, y, unit="bytes")
    )
    y += 4

    # -- Operation Performance
    panels.append(row_panel("Operation Performance", y))
    y += 1
    panels.append(
        ts(
            "Operations /s by Type",
            [_tgt(sri("vertex_db_operations_total", by="operation"), "{{operation}}")],
            0, y, w=8, unit="ops",
        )
    )
    panels.append(
        ts(
            "Operation Duration (p50/p95/p99)",
            [
                _tgt(hq(0.5, "vertex_db_operation_duration_seconds"), "p50"),
                _tgt(hq(0.95, "vertex_db_operation_duration_seconds"), "p95"),
                _tgt(hq(0.99, "vertex_db_operation_duration_seconds"), "p99"),
            ],
            8, y, w=8, unit="s",
        )
    )
    panels.append(
        ts(
            "Operations /s by Outcome",
            [_tgt(sri("vertex_db_operations_total", by="outcome"), "{{outcome}}")],
            16, y, w=8, unit="ops",
        )
    )
    y += 8

    # -- Transaction Performance
    panels.append(row_panel("Transaction Performance", y))
    y += 1
    panels.append(
        ts(
            "Transaction Duration by Mode",
            [
                _tgt(hq(0.5, "vertex_db_tx_duration_seconds", by="mode"), "p50 {{mode}}"),
                _tgt(hq(0.95, "vertex_db_tx_duration_seconds", by="mode"), "p95 {{mode}}"),
                _tgt(hq(0.99, "vertex_db_tx_duration_seconds", by="mode"), "p99 {{mode}}"),
            ],
            0, y, w=12, unit="s",
        )
    )
    panels.append(
        ts(
            "Commit Duration (p50/p95/p99)",
            [
                _tgt(hq(0.5, "vertex_db_tx_commit_duration_seconds"), "p50"),
                _tgt(hq(0.95, "vertex_db_tx_commit_duration_seconds"), "p95"),
                _tgt(hq(0.99, "vertex_db_tx_commit_duration_seconds"), "p99"),
            ],
            12, y, w=12, unit="s",
        )
    )
    y += 8

    # -- Per-Table Stats
    panels.append(row_panel("Per-Table Stats", y))
    y += 1
    panels.append(
        ts(
            "Entries per Table",
            [_tgt(f'vertex_db_entries{I}', "{{table}}")],
            0, y, w=12,
        )
    )
    panels.append(
        ts(
            "Stored Bytes per Table",
            [_tgt(f'vertex_redb_stored_bytes{I}', "{{table}}")],
            12, y, w=12, unit="bytes",
        )
    )
    y += 8
    panels.append(
        ts(
            "Metadata Bytes per Table",
            [_tgt(f'vertex_redb_metadata_bytes{I}', "{{table}}")],
            0, y, w=12, unit="bytes",
        )
    )
    panels.append(
        ts(
            "Fragmentation per Table",
            [_tgt(f'vertex_redb_fragmented_bytes{I}', "{{table}}")],
            12, y, w=12, unit="bytes",
        )
    )
    y += 8

    # -- redb Internals
    panels.append(row_panel("redb Internals", y))
    y += 1
    panels.append(
        ts(
            "Tree Height per Table",
            [_tgt(f'vertex_redb_tree_height{I}', "{{table}}")],
            0, y, w=8,
        )
    )
    panels.append(
        ts(
            "Leaf/Branch Pages per Table",
            [
                _tgt(f'vertex_redb_leaf_pages{I}', "{{table}} leaf"),
                _tgt(f'vertex_redb_branch_pages{I}', "{{table}} branch"),
            ],
            8, y, w=8,
        )
    )
    panels.append(
        ts(
            "File Size Over Time",
            [_tgt(g("vertex_redb_file_size_bytes"), "File Size")],
            16, y, w=8, unit="bytes",
        )
    )

    return dashboard(
        "vertex-database",
        "Vertex Database",
        "Database performance, storage stats, and redb internals",
        ["vertex", "database"],
        panels,
        links,
    )


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
def main():
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

    dashboards = {
        "vertex-overview.json": build_overview(),
        "vertex-topology.json": build_topology(),
        "vertex-protocols.json": build_protocols(),
        "vertex-peers.json": build_peers(),
        "vertex-database.json": build_database(),
        "vertex-system.json": build_system(),
    }

    for filename, db in dashboards.items():
        path = OUTPUT_DIR / filename
        with open(path, "w") as f:
            json.dump(db, f, indent=2)
            f.write("\n")
        panel_count = sum(1 for p in db["panels"] if p["type"] != "row")
        print(f"  {filename}: {panel_count} panels")

    print(f"\nDashboards written to {OUTPUT_DIR}")


if __name__ == "__main__":
    main()
