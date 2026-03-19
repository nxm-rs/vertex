//! Scoring methods for recording peer behaviour events.

use std::time::Duration;

use tracing::{debug, trace};
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer_score::SwarmScoringEvent;
use vertex_swarm_primitives::OverlayAddress;

use crate::entry::{on_health_changed, unix_timestamp_secs};
use crate::manager::PeerManager;

impl<I: SwarmIdentity> PeerManager<I> {
    pub fn record_latency(&self, overlay: &OverlayAddress, rtt: Duration) {
        if let Some(entry) = self.peers.get(overlay) {
            entry.set_latency(rtt);
            trace!(?overlay, ?rtt, "recorded latency");
        }
    }

    pub fn record_dial_failure(&self, overlay: &OverlayAddress) {
        if let Some(entry) = self.peers.get(overlay) {
            let old_state = entry.health_state();
            entry.record_dial_failure();
            on_health_changed(old_state, entry.health_state());
            let failures = entry.consecutive_failures();
            let backoff = entry.backoff_remaining();
            debug!(
                ?overlay,
                failures,
                backoff_secs = backoff.map(|d| d.as_secs()),
                "recorded dial failure with backoff"
            );
        } else if let Some(ref store) = self.store {
            // Cold peer - load, modify, route through write buffer
            if let Ok(Some(mut record)) = store.get(overlay) {
                record.consecutive_failures += 1;
                record.last_dial_attempt = unix_timestamp_secs();
                if self.write_buffer.push(record) {
                    self.flush_write_buffer();
                }
            }
        }
    }

    /// Record an early disconnect for a peer (post-handshake connection that failed quickly).
    pub fn record_early_disconnect(&self, overlay: &OverlayAddress, duration: Duration) {
        if let Some(entry) = self.peers.get(overlay) {
            let old_state = entry.health_state();
            entry.record_early_disconnect(duration);
            on_health_changed(old_state, entry.health_state());
            let failures = entry.consecutive_failures();
            let backoff = entry.backoff_remaining();
            debug!(
                ?overlay,
                ?duration,
                failures,
                backoff_secs = backoff.map(|d| d.as_secs()),
                "recorded early disconnect with backoff"
            );
        }
    }

    /// Record a scoring event for a peer.
    pub fn record_scoring_event(&self, overlay: &OverlayAddress, event: SwarmScoringEvent) {
        if let Some(entry) = self.peers.get(overlay) {
            entry.record_event(event);
        }
    }
}
