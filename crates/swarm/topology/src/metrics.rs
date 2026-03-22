//! Topology metrics recording for Prometheus export.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use metrics::{counter, gauge, histogram};
use vertex_observability::labels::outcome;
use vertex_observability::{
    CONNECTION_LIFETIME, DURATION_NETWORK, HistogramBucketConfig, LOCK_CONTENTION, LabelValue,
    POLL_DURATION,
};
use vertex_swarm_primitives::SwarmNodeType;

use crate::DialReason;
use crate::error::{DialError, DisconnectReason, RejectionReason};
use crate::events::{ConnectionDirection, TopologyEvent};

/// Pre-computed proximity order labels (`&'static str`) to avoid per-call allocation.
/// Covers bins 0-31 which is the full practical range for Kademlia routing.
const PO_LABELS: [&str; 32] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
    "17", "18", "19", "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "30", "31",
];

pub(crate) fn po_label(po: u8) -> &'static str {
    PO_LABELS.get(po as usize).copied().unwrap_or("overflow")
}

/// Histogram bucket configurations for topology metrics.
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[
    HistogramBucketConfig {
        suffix: "topology_connection_duration_seconds",
        buckets: CONNECTION_LIFETIME,
    },
    HistogramBucketConfig {
        suffix: "topology_dial_duration_seconds",
        buckets: DURATION_NETWORK,
    },
    // Addresses attempted per dial: integer counts (no matching preset).
    HistogramBucketConfig {
        suffix: "topology_dial_addr_count",
        buckets: &[1.0, 2.0, 3.0, 4.0, 5.0, 10.0, 15.0, 20.0, 30.0, 50.0],
    },
    // Ping RTT: 1ms to 5s (no matching preset).
    HistogramBucketConfig {
        suffix: "topology_ping_rtt_seconds",
        buckets: &[
            0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 2.5, 5.0,
        ],
    },
    HistogramBucketConfig {
        suffix: "topology_poll_duration_seconds",
        buckets: POLL_DURATION,
    },
    HistogramBucketConfig {
        suffix: "topology_routing_candidates_lock_seconds",
        buckets: LOCK_CONTENTION,
    },
    HistogramBucketConfig {
        suffix: "topology_routing_phases_lock_seconds",
        buckets: LOCK_CONTENTION,
    },
];

/// Atomically decrement a counter, clamping at zero to prevent u64 underflow.
fn saturating_decrement(counter: &AtomicU64) {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
            Some(v.saturating_sub(1))
        })
        .ok();
}

/// Metrics recorder for topology events.
pub struct TopologyMetrics {
    connected_storers: AtomicU64,
    connected_clients: AtomicU64,
    current_depth: AtomicU64,
}

impl TopologyMetrics {
    /// Create a new topology metrics recorder.
    pub fn new() -> Self {
        Self {
            connected_storers: AtomicU64::new(0),
            connected_clients: AtomicU64::new(0),
            current_depth: AtomicU64::new(0),
        }
    }

    /// Process a topology service event and record metrics.
    pub fn record_event(&self, event: &TopologyEvent) {
        match event {
            TopologyEvent::PeerReady {
                node_type,
                direction: dir,
                ..
            } => {
                self.record_peer_ready(*node_type, *dir);
            }
            TopologyEvent::PeerRejected {
                reason,
                direction: dir,
                ..
            } => {
                self.record_peer_rejected(*reason, *dir);
            }
            TopologyEvent::PeerDisconnected {
                reason,
                connection_duration,
                node_type,
                ..
            } => {
                self.record_peer_disconnected(*reason, *connection_duration, *node_type);
            }
            TopologyEvent::DepthChanged {
                old_depth,
                new_depth,
            } => {
                self.record_depth_changed(*old_depth, *new_depth);
            }
            TopologyEvent::DialFailed {
                dial_duration,
                addrs,
                reason,
                error,
                ..
            } => {
                self.record_dial_failed(*dial_duration, addrs.len(), *reason, error);
            }
            TopologyEvent::PingCompleted { rtt, .. } => {
                self.record_ping_completed(*rtt);
            }
        }
    }

    /// Record a successful peer connection.
    fn record_peer_ready(&self, node_type: SwarmNodeType, dir: ConnectionDirection) {
        let node_type_label: &'static str = if node_type.requires_storage() {
            self.connected_storers.fetch_add(1, Ordering::Relaxed);
            SwarmNodeType::Storer.into()
        } else {
            self.connected_clients.fetch_add(1, Ordering::Relaxed);
            SwarmNodeType::Client.into()
        };

        let dir_label = dir.label_value();
        self.push_connected_peer_gauges();

        // Record connection event with direction
        counter!("topology_connections_total", "node_type" => node_type_label, "direction" => dir_label, "outcome" => outcome::SUCCESS)
            .increment(1);
    }

    /// Record a rejected peer connection.
    fn record_peer_rejected(&self, reason: RejectionReason, direction: ConnectionDirection) {
        let reason_label = reason.label_value();
        let dir_label = direction.label_value();

        counter!("topology_connections_rejected_total", "reason" => reason_label, "direction" => dir_label)
            .increment(1);
    }

    /// Record a peer disconnection with node type for accurate gauge decrement.
    fn record_peer_disconnected(
        &self,
        reason: DisconnectReason,
        connection_duration: Option<Duration>,
        node_type: SwarmNodeType,
    ) {
        let reason_label = reason.label_value();
        let node_type_label: &'static str = if node_type.requires_storage() {
            saturating_decrement(&self.connected_storers);
            SwarmNodeType::Storer.into()
        } else {
            saturating_decrement(&self.connected_clients);
            SwarmNodeType::Client.into()
        };

        self.push_connected_peer_gauges();

        // Record disconnection counter with connection type and reason
        counter!(
            "topology_disconnections_total",
            "connection_type" => "peer",
            "reason" => reason_label,
            "node_type" => node_type_label,
        )
        .increment(1);

        if let Some(duration) = connection_duration {
            histogram!("topology_connection_duration_seconds", "node_type" => node_type_label)
                .record(duration.as_secs_f64());
        }
    }

    /// Record a depth change.
    fn record_depth_changed(&self, old_depth: u8, new_depth: u8) {
        self.current_depth
            .store(new_depth as u64, Ordering::Relaxed);
        gauge!("topology_depth").set(new_depth as f64);

        if new_depth > old_depth {
            counter!("topology_depth_increases_total").increment(1);
        } else {
            counter!("topology_depth_decreases_total").increment(1);
        }
    }

    /// Record a failed dial attempt.
    fn record_dial_failed(
        &self,
        dial_duration: Option<Duration>,
        addr_count: usize,
        reason: Option<DialReason>,
        error: &DialError,
    ) {
        let reason_label = reason.map(|r| r.label_value()).unwrap_or("unknown");
        let error_label = error.label_value();

        counter!("topology_dial_failures_total", "reason" => reason_label, "error_type" => error_label).increment(1);

        if let Some(duration) = dial_duration {
            histogram!("topology_dial_duration_seconds", "outcome" => outcome::FAILURE)
                .record(duration.as_secs_f64());
        }

        // Record address count for diagnostics
        histogram!("topology_dial_addr_count").record(addr_count as f64);

        // All addresses exhausted (this is the only case now)
        counter!("topology_dial_exhausted_total").increment(1);
    }

    /// Record a successful ping.
    fn record_ping_completed(&self, rtt: Duration) {
        counter!("topology_pings_total", "outcome" => outcome::SUCCESS).increment(1);
        histogram!("topology_ping_rtt_seconds").record(rtt.as_secs_f64());
    }

    /// Push both `topology_connected_peers` gauge variants from current atomics.
    fn push_connected_peer_gauges(&self) {
        let storer_label: &'static str = SwarmNodeType::Storer.into();
        let client_label: &'static str = SwarmNodeType::Client.into();
        gauge!("topology_connected_peers", "node_type" => storer_label)
            .set(self.connected_storers.load(Ordering::Relaxed) as f64);
        gauge!("topology_connected_peers", "node_type" => client_label)
            .set(self.connected_clients.load(Ordering::Relaxed) as f64);
    }

    /// Record a disconnect for a connection with unknown overlay address.
    pub fn record_unknown_overlay_disconnect(&self) {
        counter!(
            "topology_disconnections_total",
            "connection_type" => "unknown",
            "reason" => "no_overlay",
        )
        .increment(1);
    }

    /// Record an early disconnect (post-handshake connection that failed quickly).
    pub fn record_early_disconnect(&self, reason: DisconnectReason) {
        counter!(
            "topology_early_disconnects_total",
            "reason" => reason.label_value(),
        )
        .increment(1);
    }

    /// Get current depth.
    pub fn depth(&self) -> u8 {
        self.current_depth.load(Ordering::Relaxed) as u8
    }

    /// Get current connected storers count.
    pub fn connected_storers(&self) -> u64 {
        self.connected_storers.load(Ordering::Relaxed)
    }

    /// Get current connected clients count.
    pub fn connected_clients(&self) -> u64 {
        self.connected_clients.load(Ordering::Relaxed)
    }

    /// Decrement the connected counter for a replaced connection and push gauges.
    ///
    /// Called when `ActivateResult::Replaced` occurs — the old connection's `PeerReady`
    /// increment will never be balanced by a `PeerDisconnected` because the registry
    /// entry was already overwritten.
    pub fn decrement_connected(&self, node_type: SwarmNodeType) {
        if node_type.requires_storage() {
            saturating_decrement(&self.connected_storers);
        } else {
            saturating_decrement(&self.connected_clients);
        }
        self.push_connected_peer_gauges();
    }
}

/// Record connection phase transition metrics.
pub(crate) fn record_phase_transition(from: &'static str, to: &'static str) {
    counter!("topology_phase_transitions_total", "from" => from, "to" => to).increment(1);
}

/// Phase transition labels.
pub(crate) mod phase {
    pub(crate) const NONE: &str = "none";
    pub(crate) const DIALING: &str = "dialing";
    pub(crate) const HANDSHAKING: &str = "handshaking";
    pub(crate) const ACTIVE: &str = "active";
}

impl Default for TopologyMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DialError;
    use crate::events::ConnectionDirection;
    use libp2p::Multiaddr;
    use vertex_swarm_test_utils::{test_overlay, test_peer_id};

    #[test]
    fn test_metrics_new() {
        let metrics = TopologyMetrics::new();
        assert_eq!(metrics.connected_storers(), 0);
        assert_eq!(metrics.connected_clients(), 0);
        assert_eq!(metrics.depth(), 0);
    }

    #[test]
    fn test_record_peer_ready() {
        let metrics = TopologyMetrics::new();

        let event = TopologyEvent::PeerReady {
            overlay: test_overlay(0),
            peer_id: test_peer_id(1),
            node_type: SwarmNodeType::Storer,
            direction: ConnectionDirection::Outbound,
        };

        metrics.record_event(&event);
        assert_eq!(metrics.connected_storers(), 1);
        assert_eq!(metrics.connected_clients(), 0);

        let event = TopologyEvent::PeerReady {
            overlay: test_overlay(0),
            peer_id: test_peer_id(2),
            node_type: SwarmNodeType::Client,
            direction: ConnectionDirection::Inbound,
        };

        metrics.record_event(&event);
        assert_eq!(metrics.connected_storers(), 1);
        assert_eq!(metrics.connected_clients(), 1);
    }

    #[test]
    fn test_record_depth_changed() {
        let metrics = TopologyMetrics::new();

        let event = TopologyEvent::DepthChanged {
            old_depth: 0,
            new_depth: 5,
        };

        metrics.record_event(&event);
        assert_eq!(metrics.depth(), 5);

        let event = TopologyEvent::DepthChanged {
            old_depth: 5,
            new_depth: 3,
        };

        metrics.record_event(&event);
        assert_eq!(metrics.depth(), 3);
    }

    #[test]
    fn test_record_dial_failed() {
        use crate::DialReason;

        let metrics = TopologyMetrics::new();

        let event = TopologyEvent::DialFailed {
            overlay: Some(test_overlay(0)),
            addrs: vec!["/ip4/127.0.0.1/tcp/1634".parse::<Multiaddr>().unwrap()],
            error: DialError::ConnectionRefused,
            dial_duration: Some(Duration::from_secs(5)),
            reason: Some(DialReason::Discovery),
        };

        // Should not panic
        metrics.record_event(&event);
    }

    #[test]
    fn test_disconnect_without_connect_does_not_underflow() {
        let metrics = TopologyMetrics::new();
        assert_eq!(metrics.connected_clients(), 0);

        // Disconnect a client that was never connected — must not wrap to u64::MAX.
        let event = TopologyEvent::PeerDisconnected {
            overlay: test_overlay(0),
            peer_id: test_peer_id(0),
            reason: DisconnectReason::ConnectionError,
            connection_duration: None,
            node_type: SwarmNodeType::Client,
        };

        metrics.record_event(&event);
        assert_eq!(metrics.connected_clients(), 0);
        assert_eq!(metrics.connected_storers(), 0);
    }

    #[test]
    fn test_decrement_connected() {
        let metrics = TopologyMetrics::new();

        // Connect a storer and a client
        metrics.record_event(&TopologyEvent::PeerReady {
            overlay: test_overlay(1),
            peer_id: test_peer_id(1),
            node_type: SwarmNodeType::Storer,
            direction: ConnectionDirection::Outbound,
        });
        metrics.record_event(&TopologyEvent::PeerReady {
            overlay: test_overlay(2),
            peer_id: test_peer_id(2),
            node_type: SwarmNodeType::Client,
            direction: ConnectionDirection::Outbound,
        });
        assert_eq!(metrics.connected_storers(), 1);
        assert_eq!(metrics.connected_clients(), 1);

        // Simulate connection replacement — decrement for replaced storer
        metrics.decrement_connected(SwarmNodeType::Storer);
        assert_eq!(metrics.connected_storers(), 0);
        assert_eq!(metrics.connected_clients(), 1);

        // Decrement for replaced client
        metrics.decrement_connected(SwarmNodeType::Client);
        assert_eq!(metrics.connected_clients(), 0);

        // Saturates at zero
        metrics.decrement_connected(SwarmNodeType::Storer);
        assert_eq!(metrics.connected_storers(), 0);
    }

    /// Symmetric connect/disconnect with matching node types returns counters to zero.
    #[test]
    fn test_symmetric_connect_disconnect_returns_to_zero() {
        let metrics = TopologyMetrics::new();

        // Connect 3 storers and 2 clients
        for i in 0..3 {
            metrics.record_event(&TopologyEvent::PeerReady {
                overlay: test_overlay(i),
                peer_id: test_peer_id(i),
                node_type: SwarmNodeType::Storer,
                direction: ConnectionDirection::Outbound,
            });
        }
        for i in 10..12 {
            metrics.record_event(&TopologyEvent::PeerReady {
                overlay: test_overlay(i),
                peer_id: test_peer_id(i),
                node_type: SwarmNodeType::Client,
                direction: ConnectionDirection::Inbound,
            });
        }
        assert_eq!(metrics.connected_storers(), 3);
        assert_eq!(metrics.connected_clients(), 2);

        // Disconnect all with matching types
        for i in 0..3 {
            metrics.record_event(&TopologyEvent::PeerDisconnected {
                overlay: test_overlay(i),
                peer_id: test_peer_id(i),
                reason: DisconnectReason::LocalClose,
                connection_duration: Some(Duration::from_secs(60)),
                node_type: SwarmNodeType::Storer,
            });
        }
        for i in 10..12 {
            metrics.record_event(&TopologyEvent::PeerDisconnected {
                overlay: test_overlay(i),
                peer_id: test_peer_id(i),
                reason: DisconnectReason::ConnectionError,
                connection_duration: None,
                node_type: SwarmNodeType::Client,
            });
        }
        assert_eq!(metrics.connected_storers(), 0);
        assert_eq!(metrics.connected_clients(), 0);
    }

    /// Demonstrates the bug scenario: connect as Storer, disconnect as Client causes drift.
    /// The fix is in TopologyBehaviour which records node_type at PeerReady time.
    #[test]
    fn test_asymmetric_node_type_causes_drift() {
        let metrics = TopologyMetrics::new();

        // Peer connects as Storer (from handshake)
        metrics.record_event(&TopologyEvent::PeerReady {
            overlay: test_overlay(1),
            peer_id: test_peer_id(1),
            node_type: SwarmNodeType::Storer,
            direction: ConnectionDirection::Outbound,
        });
        assert_eq!(metrics.connected_storers(), 1);
        assert_eq!(metrics.connected_clients(), 0);

        // BUG SCENARIO: If gossip overwrites node_type to Client in PeerManager,
        // disconnect would decrement clients instead of storers.
        // This test documents the asymmetry — TopologyBehaviour's connected_node_types
        // HashMap prevents this by recording the type at connect time.
        metrics.record_event(&TopologyEvent::PeerDisconnected {
            overlay: test_overlay(1),
            peer_id: test_peer_id(1),
            reason: DisconnectReason::ConnectionError,
            connection_duration: Some(Duration::from_secs(120)),
            node_type: SwarmNodeType::Client, // Wrong type!
        });
        // Storer counter is now stuck at 1 (drift), client saturated at 0
        assert_eq!(metrics.connected_storers(), 1);
        assert_eq!(metrics.connected_clients(), 0);
    }

    #[test]
    fn test_record_ping_completed() {
        let metrics = TopologyMetrics::new();

        let event = TopologyEvent::PingCompleted {
            overlay: test_overlay(0),
            rtt: Duration::from_millis(50),
        };

        // Should not panic
        metrics.record_event(&event);
    }
}
