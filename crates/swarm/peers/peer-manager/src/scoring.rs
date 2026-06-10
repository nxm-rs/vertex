//! The single sanctioned peer scoring path and its non-scoring companions.
//!
//! [`PeerManager::report_peer`] is the only way any subsystem changes a
//! peer's score. It applies the event, keeps the score distribution gauges
//! consistent, and maps the resulting [`ScoreOutcome`] to lifecycle events
//! and side effects in one place.

use std::time::Duration;

use metrics::counter;
use tracing::{debug, trace, warn};
use vertex_swarm_api::{
    BanCause, DisconnectCause, PeerLifecycleEvent, PeerReporter, ReportSource, SwarmIdentity,
    SwarmScoringEvent,
};
use vertex_swarm_peer_score::ScoreOutcome;
use vertex_swarm_primitives::OverlayAddress;

use crate::entry::on_health_changed;
use crate::manager::PeerManager;

impl<I: SwarmIdentity> PeerManager<I> {
    /// Report a scoring event for a peer. THE single score-mutation path.
    ///
    /// Applies the event to the peer's score and maps the resulting
    /// [`ScoreOutcome`] in one place:
    ///
    /// - `Warn`: log and emit [`PeerLifecycleEvent::ScoreWarning`].
    /// - `Disconnect`: apply dial backoff to the peer and emit
    ///   [`PeerLifecycleEvent::DisconnectRequested`]; topology executes the
    ///   close.
    /// - `Ban`: ban the peer ([`Self::ban`]), which emits
    ///   [`PeerLifecycleEvent::Banned`].
    ///
    /// Reports for unknown peers are dropped: scoring only applies to peers
    /// the manager tracks.
    pub fn report_peer(
        &self,
        overlay: &OverlayAddress,
        event: SwarmScoringEvent,
        source: ReportSource,
    ) {
        let Some(entry) = self
            .peers
            .get(overlay)
            .map(|e| std::sync::Arc::clone(e.value()))
        else {
            trace!(?overlay, ?event, "dropping report for unknown peer");
            return;
        };

        let source_label: &'static str = source.into();
        let event_label: &'static str = event.into();

        let change = entry.record_event(event);
        self.score_distribution
            .on_score_changed(change.old_score, change.new_score);

        let outcome_label: &'static str = change.outcome.into();
        counter!(
            "peer_manager_reports_total",
            "source" => source_label,
            "event" => event_label,
            "outcome" => outcome_label,
        )
        .increment(1);
        debug!(
            ?overlay,
            event = event_label,
            source = source_label,
            score = change.new_score,
            "peer report"
        );

        match change.outcome {
            ScoreOutcome::Ok => {}
            ScoreOutcome::Warn => {
                warn!(
                    ?overlay,
                    score = change.new_score,
                    source = source_label,
                    "peer score crossed warning threshold"
                );
                self.emit(PeerLifecycleEvent::ScoreWarning {
                    overlay: *overlay,
                    score: change.new_score,
                });
            }
            ScoreOutcome::Disconnect => {
                // Back the peer off so the dialer does not immediately
                // reconnect the connection topology is about to close.
                let old_state = entry.health_state();
                entry.record_dial_failure();
                on_health_changed(old_state, entry.health_state());
                debug!(
                    ?overlay,
                    score = change.new_score,
                    source = source_label,
                    "peer score crossed disconnect threshold; requesting disconnect"
                );
                self.emit(PeerLifecycleEvent::DisconnectRequested {
                    overlay: *overlay,
                    reason: DisconnectCause::LowScore,
                });
            }
            ScoreOutcome::Ban => {
                let reason = format!(
                    "score {:+.1} at or below ban threshold after {event_label}",
                    change.new_score
                );
                warn!(
                    ?overlay,
                    score = change.new_score,
                    source = source_label,
                    "auto-banning peer"
                );
                self.ban(overlay, BanCause::LowScore, Some(reason));
            }
        }
    }

    /// Record round-trip latency for a peer without affecting its score.
    pub fn record_latency(&self, overlay: &OverlayAddress, rtt: Duration) {
        if let Some(entry) = self.peers.get(overlay) {
            entry.record_latency(rtt);
            trace!(?overlay, ?rtt, "recorded latency");
        }
    }

    /// Record a failed dial attempt: applies backoff, no score change.
    ///
    /// The scoring consequence of a dial failure (timeout, refusal) is
    /// reported separately through [`Self::report_peer`] by the caller that
    /// knows the failure class.
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
        }
    }

    /// Record an early disconnect (post-handshake connection that failed quickly).
    ///
    /// Re-arms the dial backoff (the handshake's success reset it) and
    /// reports the scoring penalty through [`Self::report_peer`].
    pub fn record_early_disconnect(&self, overlay: &OverlayAddress, duration: Duration) {
        if let Some(entry) = self.peers.get(overlay) {
            let old_state = entry.health_state();
            entry.record_dial_failure();
            on_health_changed(old_state, entry.health_state());
            debug!(
                ?overlay,
                ?duration,
                failures = entry.consecutive_failures(),
                "recorded early disconnect with backoff"
            );
        }
        self.report_peer(
            overlay,
            SwarmScoringEvent::EarlyDisconnect { duration },
            ReportSource::Topology,
        );
    }
}

impl<I: SwarmIdentity> PeerReporter for PeerManager<I> {
    fn report_peer(
        &self,
        overlay: &OverlayAddress,
        event: SwarmScoringEvent,
        source: ReportSource,
    ) {
        PeerManager::report_peer(self, overlay, event, source);
    }
}
