//! Metrics for the topology service.
//!
//! Records metrics from TopologyServiceEvent for monitoring peer connections,
//! dial attempts, handshakes, pings, and network depth.
//!
//! This module provides two patterns for recording metrics:
//!
//! - [`TopologyMetrics`]: Stateful instance that maintains gauge values (connected peers,
//!   depth) and records incremental changes. Use when you need accurate gauge tracking.
//!
//! - [`record_event`]: Stateless function that records counters and histograms only.
//!   Use for fire-and-forget metric recording when gauge accuracy isn't critical.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use metrics::{counter, gauge, histogram};
use vertex_observability::labels::outcome;
use vertex_swarm_primitives::SwarmNodeType;

use crate::DialReason;
use crate::events::{ConnectionDirection, DisconnectReason, RejectionReason, TopologyEvent};

/// Metrics recorder for topology events.
///
/// Maintains gauges for current state and records counters/histograms
/// for events as they occur.
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
                storer,
                handshake_duration,
                direction: dir,
                ..
            } => {
                self.record_peer_ready(*storer, *handshake_duration, *dir);
            }
            TopologyEvent::PeerRejected { reason, direction: dir, .. } => {
                self.record_peer_rejected(*reason, *dir);
            }
            TopologyEvent::PeerDisconnected {
                reason,
                connection_duration,
                ..
            } => {
                // We don't know if it was full/light here, so we decrement based on tracking
                // For now, just record the disconnect event
                self.record_peer_disconnected(*reason, *connection_duration);
            }
            TopologyEvent::DepthChanged { old_depth, new_depth } => {
                self.record_depth_changed(*old_depth, *new_depth);
            }
            TopologyEvent::DialFailed {
                dial_duration,
                addrs,
                reason,
                ..
            } => {
                self.record_dial_failed(*dial_duration, addrs.len(), *reason);
            }
            TopologyEvent::PingCompleted { rtt, .. } => {
                self.record_ping_completed(*rtt);
            }
        }
    }

    /// Record a successful peer connection.
    fn record_peer_ready(
        &self,
        storer: bool,
        handshake_duration: Duration,
        dir: ConnectionDirection,
    ) {
        let node_type_label: &'static str = if storer {
            self.connected_storers.fetch_add(1, Ordering::Relaxed);
            SwarmNodeType::Storer.into()
        } else {
            self.connected_clients.fetch_add(1, Ordering::Relaxed);
            SwarmNodeType::Client.into()
        };

        let dir_label = dir.as_str();
        let storer_label: &'static str = SwarmNodeType::Storer.into();
        let client_label: &'static str = SwarmNodeType::Client.into();

        // Update gauges
        gauge!("topology_connected_peers", "node_type" => storer_label)
            .set(self.connected_storers.load(Ordering::Relaxed) as f64);
        gauge!("topology_connected_peers", "node_type" => client_label)
            .set(self.connected_clients.load(Ordering::Relaxed) as f64);

        // Record connection event with direction
        counter!("topology_connections_total", "node_type" => node_type_label, "direction" => dir_label, "outcome" => outcome::SUCCESS)
            .increment(1);

        // Record handshake duration with direction
        histogram!("topology_handshake_duration_seconds", "node_type" => node_type_label, "direction" => dir_label)
            .record(handshake_duration.as_secs_f64());
    }

    /// Record a rejected peer connection.
    fn record_peer_rejected(&self, reason: RejectionReason, direction: ConnectionDirection) {
        let reason_label = reason.as_str();
        let dir_label = direction.as_str();

        counter!("topology_connections_rejected_total", "reason" => reason_label, "direction" => dir_label)
            .increment(1);
    }

    /// Record a peer disconnection.
    fn record_peer_disconnected(
        &self,
        reason: DisconnectReason,
        connection_duration: Option<Duration>,
    ) {
        let reason_label = reason.as_str();

        counter!("topology_disconnections_total", "reason" => reason_label).increment(1);

        if let Some(duration) = connection_duration {
            histogram!("topology_connection_duration_seconds").record(duration.as_secs_f64());
        }

        // Note: We can't accurately decrement full/light gauges here without tracking
        // which peer type disconnected. The PeerDisconnected event should ideally
        // include this information. For now, we rely on the caller to handle this.
    }

    /// Decrement connected peer gauge (call when you know the node type).
    pub fn record_peer_disconnected_with_type(&self, storer: bool) {
        if storer {
            self.connected_storers.fetch_sub(1, Ordering::Relaxed);
        } else {
            self.connected_clients.fetch_sub(1, Ordering::Relaxed);
        }

        let storer_label: &'static str = SwarmNodeType::Storer.into();
        let client_label: &'static str = SwarmNodeType::Client.into();

        gauge!("topology_connected_peers", "node_type" => storer_label)
            .set(self.connected_storers.load(Ordering::Relaxed) as f64);
        gauge!("topology_connected_peers", "node_type" => client_label)
            .set(self.connected_clients.load(Ordering::Relaxed) as f64);
    }

    /// Record a depth change.
    fn record_depth_changed(&self, old_depth: u8, new_depth: u8) {
        self.current_depth.store(new_depth as u64, Ordering::Relaxed);
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
    ) {
        let reason_label = reason
            .map(|r| r.as_str())
            .unwrap_or("unknown");

        counter!("topology_dial_failures_total", "reason" => reason_label).increment(1);

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

    /// Record total connected peers (for periodic gauge updates).
    pub fn set_connected_peers(&self, storers: u64, clients: u64) {
        self.connected_storers.store(storers, Ordering::Relaxed);
        self.connected_clients.store(clients, Ordering::Relaxed);

        let storer_label: &'static str = SwarmNodeType::Storer.into();
        let client_label: &'static str = SwarmNodeType::Client.into();

        gauge!("topology_connected_peers", "node_type" => storer_label)
            .set(storers as f64);
        gauge!("topology_connected_peers", "node_type" => client_label)
            .set(clients as f64);
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
}

impl Default for TopologyMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Record a topology service event (standalone function for convenience).
pub fn record_event(event: &TopologyEvent) {
    // Use a thread-local or lazy static for the metrics recorder
    // For now, just record directly without state tracking
    match event {
        TopologyEvent::PeerReady {
            storer,
            handshake_duration,
            direction: dir,
            ..
        } => {
            let node_type_label: &'static str = if *storer {
                SwarmNodeType::Storer.into()
            } else {
                SwarmNodeType::Client.into()
            };
            let dir_label = dir.as_str();

            counter!("topology_connections_total", "node_type" => node_type_label, "direction" => dir_label, "outcome" => outcome::SUCCESS)
                .increment(1);
            histogram!("topology_handshake_duration_seconds", "node_type" => node_type_label, "direction" => dir_label)
                .record(handshake_duration.as_secs_f64());
        }
        TopologyEvent::PeerRejected { reason, direction: dir, .. } => {
            let reason_label = reason.as_str();
            let dir_label = dir.as_str();
            counter!("topology_connections_rejected_total", "reason" => reason_label, "direction" => dir_label).increment(1);
        }
        TopologyEvent::PeerDisconnected {
            reason,
            connection_duration,
            ..
        } => {
            let reason_label = reason.as_str();
            counter!("topology_disconnections_total", "reason" => reason_label).increment(1);

            if let Some(duration) = connection_duration {
                histogram!("topology_connection_duration_seconds").record(duration.as_secs_f64());
            }
        }
        TopologyEvent::DepthChanged { new_depth, .. } => {
            gauge!("topology_depth").set(*new_depth as f64);
        }
        TopologyEvent::DialFailed {
            dial_duration,
            addrs,
            reason,
            ..
        } => {
            let reason_label = reason
                .map(|r| r.as_str())
                .unwrap_or("unknown");

            counter!("topology_dial_failures_total", "reason" => reason_label).increment(1);

            if let Some(duration) = dial_duration {
                histogram!("topology_dial_duration_seconds", "outcome" => outcome::FAILURE)
                    .record(duration.as_secs_f64());
            }

            histogram!("topology_dial_addr_count").record(addrs.len() as f64);
            counter!("topology_dial_exhausted_total").increment(1);
        }
        TopologyEvent::PingCompleted { rtt, .. } => {
            counter!("topology_pings_total", "outcome" => outcome::SUCCESS).increment(1);
            histogram!("topology_ping_rtt_seconds").record(rtt.as_secs_f64());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ConnectionDirection;
    use libp2p::{Multiaddr, PeerId};
    use vertex_swarm_primitives::OverlayAddress;

    fn test_overlay() -> OverlayAddress {
        OverlayAddress::default()
    }

    fn test_peer_id() -> PeerId {
        PeerId::random()
    }

    fn test_addr() -> Multiaddr {
        "/ip4/127.0.0.1/tcp/1634".parse().unwrap()
    }

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
            overlay: test_overlay(),
            peer_id: test_peer_id(),
            storer: true,
            handshake_duration: Duration::from_millis(150),
            direction: ConnectionDirection::Outbound,
        };

        metrics.record_event(&event);
        assert_eq!(metrics.connected_storers(), 1);
        assert_eq!(metrics.connected_clients(), 0);

        let event = TopologyEvent::PeerReady {
            overlay: test_overlay(),
            peer_id: test_peer_id(),
            storer: false,
            handshake_duration: Duration::from_millis(200),
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
            overlay: Some(test_overlay()),
            addrs: vec![test_addr()],
            error: "connection refused".to_string(),
            dial_duration: Some(Duration::from_secs(5)),
            reason: Some(DialReason::Discovery),
        };

        // Should not panic
        metrics.record_event(&event);
    }

    #[test]
    fn test_record_ping_completed() {
        let metrics = TopologyMetrics::new();

        let event = TopologyEvent::PingCompleted {
            overlay: test_overlay(),
            rtt: Duration::from_millis(50),
        };

        // Should not panic
        metrics.record_event(&event);
    }

    #[test]
    fn test_standalone_record_event() {
        // Test the standalone function
        let event = TopologyEvent::PeerReady {
            overlay: test_overlay(),
            peer_id: test_peer_id(),
            storer: true,
            handshake_duration: Duration::from_millis(100),
            direction: ConnectionDirection::Outbound,
        };

        // Should not panic
        record_event(&event);
    }
}
