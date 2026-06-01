//! Per-peer stabilization detector.
//!
//! Bee's `stabilization.Detector` is a global rate-stability gate; vertex
//! re-frames the concept per peer for the pingpong reachability bridge in
//! Unit 10. A peer is considered "stable" once we have observed a configured
//! number of consecutive successful ping/pong exchanges within a sliding time
//! window. Stability decays as soon as a sample fails or as soon as the
//! window expires without new observations.
//!
//! The default parameters (`N = 3`, `T = 30s`) match the bee
//! `Config{ NumPeriodsForStabilization: 3, PeriodDuration: 10s }` setup used
//! by `pkg/topology/kademlia` (see `kademlia.go:204`).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use libp2p::PeerId;

/// Default number of consecutive successful observations required for a peer
/// to be considered stable.
pub const DEFAULT_REQUIRED_OK: u32 = 3;

/// Default time window inside which the consecutive successes must land.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(30);

/// A pluggable detector that decides when a peer's behaviour has stabilized
/// enough to be trusted by upper layers (e.g. reachability bridge).
///
/// Implementations must be `Send + Sync + 'static` so the detector can be
/// shared with the topology behaviour and with background tasks.
pub trait StabilizationDetector: Send + Sync + 'static {
    /// Record an observation for `peer`. `ok = true` indicates a successful
    /// pingpong exchange; `ok = false` indicates a failure (timeout,
    /// protocol error, …). `rtt` is the observed round-trip time; it is
    /// ignored on failure.
    fn observe(&mut self, peer: PeerId, ok: bool, rtt: Duration);

    /// Returns `true` if the detector currently considers `peer` stable.
    fn is_stable(&self, peer: &PeerId) -> bool;
}

/// Builder for [`ConsecutiveOkDetector`].
#[derive(Debug, Clone)]
pub struct StabilizationConfig {
    required_ok: u32,
    window: Duration,
}

impl Default for StabilizationConfig {
    fn default() -> Self {
        Self {
            required_ok: DEFAULT_REQUIRED_OK,
            window: DEFAULT_WINDOW,
        }
    }
}

impl StabilizationConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the number of consecutive successes required for stability.
    #[must_use]
    pub fn with_required_ok(mut self, required: u32) -> Self {
        self.required_ok = required.max(1);
        self
    }

    /// Set the sliding time window inside which successes must land.
    #[must_use]
    pub fn with_window(mut self, window: Duration) -> Self {
        self.window = window;
        self
    }

    #[must_use]
    pub fn required_ok(&self) -> u32 {
        self.required_ok
    }

    #[must_use]
    pub fn window(&self) -> Duration {
        self.window
    }

    /// Build a concrete detector with these settings.
    #[must_use]
    pub fn build(self) -> ConsecutiveOkDetector {
        ConsecutiveOkDetector::with_config(self)
    }
}

#[derive(Debug, Clone, Copy)]
struct PeerState {
    /// Consecutive successful observations.
    consecutive_ok: u32,
    /// Instant of the first success in the current streak.
    streak_started_at: Instant,
    /// Instant of the most recent successful observation.
    last_ok_at: Instant,
}

/// Default [`StabilizationDetector`] implementation.
///
/// Tracks consecutive successful pingpong exchanges per peer and considers a
/// peer stable once the number of consecutive successes reaches
/// [`StabilizationConfig::required_ok`] within a sliding
/// [`StabilizationConfig::window`].
///
/// Any failed observation, any observation older than the window, or a
/// missing observation (gap larger than the window) resets the streak.
pub struct ConsecutiveOkDetector {
    config: StabilizationConfig,
    peers: Mutex<HashMap<PeerId, PeerState>>,
}

impl ConsecutiveOkDetector {
    /// Construct with default settings ([`StabilizationConfig::default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(StabilizationConfig::default())
    }

    /// Construct from an explicit configuration.
    #[must_use]
    pub fn with_config(config: StabilizationConfig) -> Self {
        Self {
            config,
            peers: Mutex::new(HashMap::new()),
        }
    }

    /// Read-only access to the active configuration.
    #[must_use]
    pub fn config(&self) -> &StabilizationConfig {
        &self.config
    }

    /// Drop tracking state for `peer`. Call on disconnect to avoid unbounded
    /// growth. Returns `true` if a state entry was removed.
    pub fn forget(&self, peer: &PeerId) -> bool {
        match self.peers.lock() {
            Ok(mut map) => map.remove(peer).is_some(),
            Err(poisoned) => {
                // Mutex poisoning here is non-fatal; we recover the state.
                let mut map = poisoned.into_inner();
                map.remove(peer).is_some()
            }
        }
    }

    /// Test-only helper: returns the current streak length for `peer`.
    #[cfg(test)]
    fn streak(&self, peer: &PeerId) -> u32 {
        self.peers
            .lock()
            .ok()
            .and_then(|m| m.get(peer).map(|s| s.consecutive_ok))
            .unwrap_or(0)
    }
}

impl Default for ConsecutiveOkDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl StabilizationDetector for ConsecutiveOkDetector {
    fn observe(&mut self, peer: PeerId, ok: bool, _rtt: Duration) {
        let now = Instant::now();
        let mut peers = match self.peers.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };

        if !ok {
            peers.remove(&peer);
            return;
        }

        let entry = peers.entry(peer).or_insert_with(|| PeerState {
            consecutive_ok: 0,
            streak_started_at: now,
            last_ok_at: now,
        });

        // If the previous success is older than the window, restart the streak.
        if now.duration_since(entry.last_ok_at) > self.config.window {
            *entry = PeerState {
                consecutive_ok: 1,
                streak_started_at: now,
                last_ok_at: now,
            };
            return;
        }

        // If the running streak already spans more than the window, slide.
        if now.duration_since(entry.streak_started_at) > self.config.window {
            *entry = PeerState {
                consecutive_ok: 1,
                streak_started_at: now,
                last_ok_at: now,
            };
            return;
        }

        entry.consecutive_ok = entry.consecutive_ok.saturating_add(1);
        entry.last_ok_at = now;
    }

    fn is_stable(&self, peer: &PeerId) -> bool {
        let peers = match self.peers.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let Some(state) = peers.get(peer) else {
            return false;
        };
        let now = Instant::now();
        // Stable status decays if no successful observation lands within the
        // configured window.
        if now.duration_since(state.last_ok_at) > self.config.window {
            return false;
        }
        state.consecutive_ok >= self.config.required_ok
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn peer_id() -> PeerId {
        PeerId::random()
    }

    #[test]
    fn defaults_match_plan_constants() {
        let cfg = StabilizationConfig::default();
        assert_eq!(cfg.required_ok(), 3);
        assert_eq!(cfg.window(), Duration::from_secs(30));
    }

    #[test]
    fn unobserved_peer_is_not_stable() {
        let det = ConsecutiveOkDetector::new();
        assert!(!det.is_stable(&peer_id()));
    }

    #[test]
    fn becomes_stable_after_required_consecutive_successes() {
        let mut det = ConsecutiveOkDetector::new();
        let p = peer_id();
        assert!(!det.is_stable(&p));
        for _ in 0..DEFAULT_REQUIRED_OK {
            det.observe(p, true, Duration::from_millis(10));
        }
        assert!(det.is_stable(&p));
    }

    #[test]
    fn failure_resets_streak() {
        let mut det = ConsecutiveOkDetector::new();
        let p = peer_id();
        for _ in 0..DEFAULT_REQUIRED_OK {
            det.observe(p, true, Duration::from_millis(10));
        }
        assert!(det.is_stable(&p));

        det.observe(p, false, Duration::ZERO);
        assert!(!det.is_stable(&p));
        assert_eq!(det.streak(&p), 0);
    }

    #[test]
    fn builder_overrides_take_effect() {
        let det = StabilizationConfig::new()
            .with_required_ok(2)
            .with_window(Duration::from_millis(5))
            .build();
        assert_eq!(det.config().required_ok(), 2);
        assert_eq!(det.config().window(), Duration::from_millis(5));
    }

    #[test]
    fn lower_required_ok_threshold_is_respected() {
        let mut det = StabilizationConfig::new().with_required_ok(2).build();
        let p = peer_id();
        det.observe(p, true, Duration::from_millis(1));
        assert!(!det.is_stable(&p));
        det.observe(p, true, Duration::from_millis(1));
        assert!(det.is_stable(&p));
    }

    #[test]
    fn forget_drops_state() {
        let mut det = ConsecutiveOkDetector::new();
        let p = peer_id();
        det.observe(p, true, Duration::from_millis(1));
        assert_eq!(det.streak(&p), 1);
        assert!(det.forget(&p));
        assert_eq!(det.streak(&p), 0);
    }

    #[test]
    fn required_ok_floor_is_one() {
        // Builder must not allow zero — otherwise every peer would be stable
        // immediately without any observations.
        let cfg = StabilizationConfig::new().with_required_ok(0);
        assert_eq!(cfg.required_ok(), 1);
    }
}
