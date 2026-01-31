//! Per-peer score handle for lock-free event recording.
//!
//! The `ScoreHandle` wraps an `Arc<PeerScoreState>` and provides a convenient
//! API for recording events. Handles are cheap to clone (just Arc refs) and
//! can be stored in per-peer state within vertex-swarm-client.

use std::sync::Arc;

use vertex_primitives::OverlayAddress;

use super::config::ScoreWeights;
use super::event::ScoreEvent;
use super::peer::PeerScoreState;

/// Handle for recording score events on a single peer.
///
/// This handle is cheap to clone (two Arc refs) and can be held by
/// per-peer connection state in vertex-swarm-client. Multiple protocol
/// handlers can clone and use the same handle concurrently without
/// mutex contention.
#[derive(Clone)]
pub struct ScoreHandle {
    state: Arc<PeerScoreState>,
    weights: Arc<ScoreWeights>,
}

impl ScoreHandle {
    /// Create a new handle wrapping the given state.
    pub fn new(state: Arc<PeerScoreState>, weights: Arc<ScoreWeights>) -> Self {
        Self { state, weights }
    }

    /// Get the peer's overlay address.
    pub fn peer(&self) -> OverlayAddress {
        self.state.peer()
    }

    /// Get the current score.
    pub fn score(&self) -> f64 {
        self.state.score()
    }

    /// Record an event and update the score.
    ///
    /// This is lock-free and can be called concurrently from multiple threads.
    pub fn record(&self, event: ScoreEvent) {
        let delta = self.event_to_delta(&event);
        self.state.add_score(delta);
        self.update_counters(&event);
    }

    /// Record a connection success with latency measurement.
    pub fn record_connection_success(&self, latency_ms: u32) {
        self.state.add_score(self.weights.connection_success);
        self.state.record_connection_success();
        self.state.record_latency(latency_ms);
    }

    /// Record a connection timeout.
    pub fn record_connection_timeout(&self) {
        self.state.add_score(self.weights.connection_timeout);
        self.state.record_connection_timeout();
    }

    /// Record a connection refusal.
    pub fn record_connection_refused(&self) {
        self.state.add_score(self.weights.connection_refused);
        self.state.record_connection_refused();
    }

    /// Record a handshake failure.
    pub fn record_handshake_failure(&self) {
        self.state.add_score(self.weights.handshake_failure);
        self.state.record_handshake_failure();
    }

    /// Record a protocol error.
    pub fn record_protocol_error(&self) {
        self.state.add_score(self.weights.protocol_error);
        self.state.record_protocol_error();
    }

    /// Record a successful chunk delivery.
    pub fn record_chunk_delivered(&self, latency_ms: u32) {
        let delta = if latency_ms < 100 {
            self.weights.chunk_delivered_fast
        } else {
            self.weights.chunk_delivered
        };
        self.state.add_score(delta);
        self.state.record_chunk_delivered();
        self.state.record_latency(latency_ms);
    }

    /// Record an invalid chunk.
    pub fn record_invalid_chunk(&self) {
        self.state.add_score(self.weights.invalid_chunk);
        self.state.record_invalid_chunk();
    }

    /// Record protocol compliance (small positive signal).
    pub fn record_protocol_compliance(&self) {
        self.state.add_score(self.weights.protocol_compliance);
    }

    /// Record a slow response.
    pub fn record_slow_response(&self) {
        self.state.add_score(self.weights.slow_response);
    }

    /// Apply a manual score adjustment.
    pub fn adjust(&self, delta: f64) {
        self.state.add_score(delta);
    }

    /// Check if the peer should be banned based on score.
    pub fn should_ban(&self, threshold: f64) -> bool {
        self.state.should_ban(threshold)
    }

    /// Check if the peer should be deprioritized.
    pub fn should_deprioritize(&self, threshold: f64) -> bool {
        self.state.should_deprioritize(threshold)
    }

    /// Get the success rate for this peer.
    pub fn success_rate(&self) -> f64 {
        self.state.success_rate()
    }

    /// Get the average latency for this peer.
    pub fn avg_latency_ms(&self) -> Option<u32> {
        self.state.avg_latency_ms()
    }

    /// Get a reference to the underlying state.
    pub fn state(&self) -> &Arc<PeerScoreState> {
        &self.state
    }

    fn event_to_delta(&self, event: &ScoreEvent) -> f64 {
        match event {
            ScoreEvent::ConnectionSuccess => self.weights.connection_success,
            ScoreEvent::ConnectionTimeout => self.weights.connection_timeout,
            ScoreEvent::ConnectionRefused => self.weights.connection_refused,
            ScoreEvent::HandshakeFailure => self.weights.handshake_failure,
            ScoreEvent::ProtocolCompliance => self.weights.protocol_compliance,
            ScoreEvent::ProtocolError => self.weights.protocol_error,
            ScoreEvent::ChunkDelivered { latency_ms } => {
                if *latency_ms < 100 {
                    self.weights.chunk_delivered_fast
                } else {
                    self.weights.chunk_delivered
                }
            }
            ScoreEvent::InvalidChunk => self.weights.invalid_chunk,
            ScoreEvent::SlowResponse => self.weights.slow_response,
            ScoreEvent::ManualBoost(n) => *n as f64,
            ScoreEvent::ManualPenalty(n) => -(*n as f64),
        }
    }

    fn update_counters(&self, event: &ScoreEvent) {
        match event {
            ScoreEvent::ConnectionSuccess => self.state.record_connection_success(),
            ScoreEvent::ConnectionTimeout => self.state.record_connection_timeout(),
            ScoreEvent::ConnectionRefused => self.state.record_connection_refused(),
            ScoreEvent::HandshakeFailure => self.state.record_handshake_failure(),
            ScoreEvent::ProtocolError => self.state.record_protocol_error(),
            ScoreEvent::ChunkDelivered { latency_ms } => {
                self.state.record_chunk_delivered();
                self.state.record_latency(*latency_ms);
            }
            ScoreEvent::InvalidChunk => self.state.record_invalid_chunk(),
            _ => {}
        }
    }
}

impl std::fmt::Debug for ScoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScoreHandle")
            .field("peer", &self.state.peer())
            .field("score", &self.state.score())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    fn test_overlay(n: u8) -> OverlayAddress {
        OverlayAddress::from(B256::repeat_byte(n))
    }

    fn test_handle(n: u8) -> ScoreHandle {
        let state = Arc::new(PeerScoreState::new(test_overlay(n)));
        let weights = Arc::new(ScoreWeights::default());
        ScoreHandle::new(state, weights)
    }

    #[test]
    fn test_handle_clone() {
        let handle1 = test_handle(1);
        let handle2 = handle1.clone();

        // Both handles share the same state
        handle1.record_connection_success(50);
        assert_eq!(handle2.state.connection_successes(), 1);
        assert!(handle2.score() > 0.0);
    }

    #[test]
    fn test_record_events() {
        let handle = test_handle(1);

        handle.record(ScoreEvent::ConnectionSuccess);
        assert!(handle.score() > 0.0);

        handle.record(ScoreEvent::ProtocolError);
        // Protocol error has higher weight, so score should be negative
        assert!(handle.score() < 0.0);
    }

    #[test]
    fn test_typed_record_methods() {
        let handle = test_handle(1);

        handle.record_connection_success(50);
        assert_eq!(handle.state.connection_successes(), 1);
        assert_eq!(handle.avg_latency_ms(), Some(50));

        handle.record_protocol_error();
        assert_eq!(handle.state.protocol_errors(), 1);
    }

    #[test]
    fn test_chunk_delivered_fast_vs_slow() {
        let fast_handle = test_handle(1);
        let slow_handle = test_handle(2);

        fast_handle.record_chunk_delivered(50); // Fast (< 100ms)
        slow_handle.record_chunk_delivered(150); // Slow (>= 100ms)

        // Fast delivery should give higher score
        assert!(fast_handle.score() > slow_handle.score());
    }

    #[test]
    fn test_concurrent_handles() {
        use std::thread;

        let state = Arc::new(PeerScoreState::new(test_overlay(1)));
        let weights = Arc::new(ScoreWeights::default());

        let mut handles = vec![];
        for _ in 0..4 {
            let handle = ScoreHandle::new(Arc::clone(&state), Arc::clone(&weights));
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    handle.record_connection_success(50);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(state.connection_successes(), 400);
    }
}
