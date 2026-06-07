//! Per-peer reachability tracker.
//!
//! Two upstream-canonical liveness signals feed a single per-peer verdict that
//! the kademlia routing layer consumes when picking eviction candidates and
//! counting bin saturation:
//!
//! * **`libp2p::ping`** is the primary signal: a successful ping promotes a
//!   peer to `Public`; failed pings accumulate and, once the count crosses
//!   [`FAILURE_THRESHOLD`] within [`FAILURE_DECAY`], flip it to `Private`. This
//!   mirrors the reference implementation's reacher, which judges per-peer
//!   reachability by pinging over `/ipfs/ping`.
//! * **Handshake faults** share the same streak counter (a protocol-fault is a
//!   negative signal). The decay window keeps historical failures from
//!   outweighing recent evidence; ambiguous errors (timeouts,
//!   connection-closed-by-either-side, IO) are filtered upstream and never
//!   reach the tracker. A single negative signal never blacklists a peer.
//!
//! * **AutoNAT v2 dial-back** is the third positive signal. When our node acts
//!   as an AutoNAT v2 server and successfully dials a peer back, that peer is
//!   proven publicly reachable; the node wiring forwards the confirmed peer via
//!   [`Self::on_autonat_peer_confirmed`], which promotes it to `Public`.
//!
//! Records are dropped on `ConnectionClosed` so memory does not accumulate
//! for transient or scanner peers; the [`Self::forget`] entry point is
//! called from the topology behaviour's disconnect handler.
//!
//! Concurrency: a single `Arc<parking_lot::RwLock<HashMap<PeerId, Record>>>`.
//! Reads dominate; the RwLock never poisons.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use libp2p::PeerId;
use parking_lot::RwLock;
use tracing::{debug, trace};

/// Number of consecutive negative liveness signals (failed ping or handshake
/// fault) within [`FAILURE_DECAY`] that flip a peer to
/// [`PeerReachability::Private`]. Picked on the lenient side so that
/// transient network blips do not blacklist peers.
pub const FAILURE_THRESHOLD: u32 = 3;

/// How long after the first counted failure the running count is allowed to
/// grow before it resets. Three failures spread over weeks no longer
/// constitute evidence; the decay window forces them to be recent.
pub const FAILURE_DECAY: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Reachability status of an individual peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum PeerReachability {
    /// No signal yet — treated as neutral.
    #[default]
    Unknown,
    /// Peer is reachable from the public internet.
    Public,
    /// Peer is behind NAT or otherwise unreachable.
    Private,
}

impl PeerReachability {
    /// `true` if status is [`PeerReachability::Public`].
    pub fn is_public(&self) -> bool {
        matches!(self, Self::Public)
    }

    /// `true` if status is [`PeerReachability::Private`].
    pub fn is_private(&self) -> bool {
        matches!(self, Self::Private)
    }

    /// Eviction ordering: higher rank survives. `Public` > `Unknown` >
    /// `Private`.
    pub fn rank(&self) -> u8 {
        match self {
            Self::Public => 2,
            Self::Unknown => 1,
            Self::Private => 0,
        }
    }
}

/// Per-peer record kept inside [`ReachabilityTracker`].
#[derive(Debug, Clone, Copy)]
struct PeerReachabilityRecord {
    status: PeerReachability,
    /// Last time the `status` *transitioned* to a different value. Re-affirming
    /// the current status (e.g. repeated handshake successes for a Public
    /// peer) does **not** bump this timestamp.
    last_updated: Instant,
    /// Consecutive negative liveness signals (failed ping or handshake fault)
    /// since the last success or decay-window reset.
    failures: u32,
    /// Time of the first failure in the current run. Used to decay the
    /// counter once [`FAILURE_DECAY`] has elapsed without crossing
    /// the threshold.
    first_failure_at: Option<Instant>,
}

impl PeerReachabilityRecord {
    fn new(status: PeerReachability) -> Self {
        Self {
            status,
            last_updated: Instant::now(),
            failures: 0,
            first_failure_at: None,
        }
    }

    /// Set the status and bump `last_updated`. Returns `true` if the status
    /// actually changed.
    fn set_status(&mut self, status: PeerReachability) -> bool {
        if self.status == status {
            return false;
        }
        self.status = status;
        self.last_updated = Instant::now();
        true
    }

    /// Reset the failure run.
    fn clear_failures(&mut self) {
        self.failures = 0;
        self.first_failure_at = None;
    }
}

/// Thread-safe per-peer reachability tracker.
///
/// Cheap to clone; internally an `Arc<RwLock<...>>`. Never panics —
/// `parking_lot::RwLock` does not poison and all map accesses go through
/// `entry`/`get`.
#[derive(Debug, Clone, Default)]
pub struct ReachabilityTracker {
    inner: Arc<RwLock<HashMap<PeerId, PeerReachabilityRecord>>>,
}

impl ReachabilityTracker {
    /// Create an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current reachability for `peer`, or [`PeerReachability::Unknown`] if no
    /// signal has been observed yet.
    pub fn status(&self, peer: &PeerId) -> PeerReachability {
        self.inner
            .read()
            .get(peer)
            .map(|r| r.status)
            .unwrap_or(PeerReachability::Unknown)
    }

    /// `true` if a record exists for `peer` (any status).
    pub fn contains(&self, peer: &PeerId) -> bool {
        self.inner.read().contains_key(peer)
    }

    /// Number of tracked peers.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// `true` if no peers are tracked.
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Drop the record for `peer`. Returns the prior status if one was tracked.
    pub fn forget(&self, peer: &PeerId) -> Option<PeerReachability> {
        self.inner.write().remove(peer).map(|r| r.status)
    }

    /// Timestamp of the most recent **status transition** for `peer`. This is
    /// not the time of the last signal; re-affirming the existing status
    /// (e.g. successive handshake successes for a Public peer) leaves it
    /// unchanged. Returns `None` if `peer` is untracked.
    pub fn last_updated(&self, peer: &PeerId) -> Option<Instant> {
        self.inner.read().get(peer).map(|r| r.last_updated)
    }

    /// Record that a peer has been confirmed publicly reachable via an
    /// AutoNAT v2 dial-back.
    ///
    /// Called by the node wiring for each successful
    /// `autonat::v2::server::Event` (a dial-back our server completed against
    /// the peer's advertised address). A completed dial-back proves the peer
    /// accepts inbound connections on that address, so we promote it to
    /// [`PeerReachability::Public`].
    pub fn on_autonat_peer_confirmed(&self, peer: PeerId) {
        self.set_public(peer, "autonat");
    }

    /// Update from a `libp2p::ping` round-trip outcome.
    ///
    /// This is the primary liveness signal, mirroring the reference
    /// implementation's reacher (which pings over `/ipfs/ping`): a successful
    /// ping promotes the peer to [`PeerReachability::Public`]; failed pings
    /// accumulate and, once [`FAILURE_THRESHOLD`] consecutive failures land
    /// within [`FAILURE_DECAY`], flip it to [`PeerReachability::Private`].
    pub fn update_from_ping(&self, peer: PeerId, success: bool) {
        self.record_liveness(peer, success, "ping");
    }

    /// Update from a handshake outcome.
    ///
    /// A connection-time signal sharing the same streak mechanism as
    /// [`update_from_ping`](Self::update_from_ping): success promotes, repeated
    /// faults demote. In practice topology feeds only handshake *faults* here
    /// (ongoing promotion is owned by ping).
    pub fn update_from_handshake(&self, peer: PeerId, success: bool) {
        self.record_liveness(peer, success, "handshake");
    }

    // ---------------------------------------------------------------------
    // Internal helpers
    // ---------------------------------------------------------------------

    fn set_public(&self, peer: PeerId, source: &'static str) {
        let mut guard = self.inner.write();
        let entry = guard
            .entry(peer)
            .or_insert_with(|| PeerReachabilityRecord::new(PeerReachability::Unknown));
        entry.failures = 0;
        if entry.set_status(PeerReachability::Public) {
            debug!(%peer, source, "peer reachability set to Public");
        }
    }

    /// Shared streak logic for the two liveness signals (ping, handshake).
    ///
    /// Success promotes to `Public` and clears the failure run. A failure
    /// accumulates; the run decays if its first failure predates
    /// [`FAILURE_DECAY`], and the peer flips to `Private` once
    /// [`FAILURE_THRESHOLD`] consecutive failures land inside the window. A
    /// single negative signal never blacklists a peer.
    fn record_liveness(&self, peer: PeerId, success: bool, source: &'static str) {
        let mut guard = self.inner.write();
        let entry = guard
            .entry(peer)
            .or_insert_with(|| PeerReachabilityRecord::new(PeerReachability::Unknown));

        if success {
            entry.clear_failures();
            if entry.set_status(PeerReachability::Public) {
                debug!(%peer, source, "peer reachability set to Public");
            } else {
                trace!(%peer, source, "liveness success reaffirms Public");
            }
            return;
        }

        let now = Instant::now();
        // Decay the run if the first failure is too far in the past; stale
        // historical failures must not cumulatively cross the threshold.
        if let Some(start) = entry.first_failure_at
            && now.duration_since(start) > FAILURE_DECAY
        {
            entry.clear_failures();
        }
        if entry.first_failure_at.is_none() {
            entry.first_failure_at = Some(now);
        }
        entry.failures = entry.failures.saturating_add(1);
        if entry.failures >= FAILURE_THRESHOLD && entry.set_status(PeerReachability::Private) {
            debug!(
                %peer,
                source,
                failures = entry.failures,
                "peer reachability set to Private after repeated failures"
            );
        } else {
            trace!(%peer, source, failures = entry.failures, "liveness failure noted");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_peer() -> PeerId {
        PeerId::random()
    }

    #[test]
    fn unknown_by_default() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        assert_eq!(tracker.status(&peer), PeerReachability::Unknown);
        assert!(!tracker.contains(&peer));
        assert!(tracker.is_empty());
    }

    #[test]
    fn handshake_success_promotes_to_public() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_handshake(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
        assert!(tracker.contains(&peer));
        assert_eq!(tracker.len(), 1);
    }

    #[test]
    fn single_handshake_failure_stays_unknown() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_handshake(peer, false);
        assert_eq!(tracker.status(&peer), PeerReachability::Unknown);
    }

    #[test]
    fn repeated_handshake_failures_flip_to_private() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        for _ in 0..FAILURE_THRESHOLD {
            tracker.update_from_handshake(peer, false);
        }
        assert_eq!(tracker.status(&peer), PeerReachability::Private);
    }

    #[test]
    fn handshake_success_resets_failure_counter() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        for _ in 0..(FAILURE_THRESHOLD - 1) {
            tracker.update_from_handshake(peer, false);
        }
        tracker.update_from_handshake(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
        // Now a single failure must NOT flip back to Private.
        tracker.update_from_handshake(peer, false);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
    }

    #[test]
    fn ping_success_promotes_to_public() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_ping(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
    }

    #[test]
    fn repeated_ping_failures_flip_to_private() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        for _ in 0..FAILURE_THRESHOLD {
            tracker.update_from_ping(peer, false);
        }
        assert_eq!(tracker.status(&peer), PeerReachability::Private);
    }

    #[test]
    fn ping_success_resets_failure_streak() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        for _ in 0..(FAILURE_THRESHOLD - 1) {
            tracker.update_from_ping(peer, false);
        }
        tracker.update_from_ping(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
        // One further failure must not flip back.
        tracker.update_from_ping(peer, false);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
    }

    #[test]
    fn ping_and_handshake_share_one_failure_streak() {
        // Both signals feed the same counter; mixed failures cross the
        // threshold together.
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_handshake(peer, false);
        for _ in 0..(FAILURE_THRESHOLD - 1) {
            tracker.update_from_ping(peer, false);
        }
        assert_eq!(tracker.status(&peer), PeerReachability::Private);
    }

    #[test]
    fn forget_removes_record() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_handshake(peer, true);
        let dropped = tracker.forget(&peer);
        assert_eq!(dropped, Some(PeerReachability::Public));
        assert!(!tracker.contains(&peer));
        assert_eq!(tracker.status(&peer), PeerReachability::Unknown);
    }

    #[test]
    fn forget_returns_none_for_unknown_peer() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        assert!(tracker.forget(&peer).is_none());
    }

    #[test]
    fn rank_orders_public_above_unknown_above_private() {
        // The eviction tiebreak relies on this ordering: least-reachable
        // (lowest rank) is evicted first.
        let tracker = ReachabilityTracker::new();
        let public_peer = random_peer();
        let private_peer = random_peer();
        let unknown_peer = random_peer();

        tracker.update_from_handshake(public_peer, true);
        for _ in 0..FAILURE_THRESHOLD {
            tracker.update_from_handshake(private_peer, false);
        }

        assert!(tracker.status(&public_peer).rank() > tracker.status(&unknown_peer).rank());
        assert!(tracker.status(&unknown_peer).rank() > tracker.status(&private_peer).rank());
    }

    #[test]
    fn autonat_peer_confirmed_promotes_to_public() {
        // A successful AutoNAT v2 dial-back promotes the verified peer; the
        // node wiring forwards each such event through this entry point.
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.on_autonat_peer_confirmed(peer);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
    }

    #[test]
    fn autonat_peer_confirmed_clears_handshake_failures() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        for _ in 0..(FAILURE_THRESHOLD - 1) {
            tracker.update_from_handshake(peer, false);
        }
        tracker.on_autonat_peer_confirmed(peer);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
        // Failure counter is reset — a single further failure must not flip.
        tracker.update_from_handshake(peer, false);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
    }

    #[test]
    fn failure_counter_decays_after_window() {
        // Three failures spaced over a long window must not cumulatively
        // cross the threshold; only failures within FAILURE_DECAY
        // of each other count.
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_handshake(peer, false);

        // Backdate the record's first failure so the next call sees it
        // outside the decay window.
        {
            let mut guard = tracker.inner.write();
            let entry = guard.get_mut(&peer).expect("entry exists after failure");
            entry.first_failure_at = Some(Instant::now() - FAILURE_DECAY * 2);
        }

        for _ in 0..(FAILURE_THRESHOLD - 1) {
            tracker.update_from_handshake(peer, false);
        }
        // The first failure decayed; only FAILURE_THRESHOLD - 1
        // recent failures remain, so the peer stays Unknown.
        assert_eq!(tracker.status(&peer), PeerReachability::Unknown);
    }

    #[test]
    fn ranks_total_order() {
        assert!(PeerReachability::Public.rank() > PeerReachability::Unknown.rank());
        assert!(PeerReachability::Unknown.rank() > PeerReachability::Private.rank());
    }

    #[test]
    fn cloned_tracker_shares_state() {
        let tracker = ReachabilityTracker::new();
        let clone = tracker.clone();
        let peer = random_peer();
        tracker.update_from_handshake(peer, true);
        assert_eq!(clone.status(&peer), PeerReachability::Public);
        assert_eq!(clone.len(), 1);
    }

    #[test]
    fn last_updated_changes_on_status_change_only() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_handshake(peer, true);
        let Some(t1) = tracker.last_updated(&peer) else {
            panic!("expected last_updated to be set after handshake success");
        };

        // Reaffirming Public does not change `last_updated`.
        std::thread::sleep(std::time::Duration::from_millis(2));
        tracker.update_from_handshake(peer, true);
        let Some(t2) = tracker.last_updated(&peer) else {
            panic!("expected last_updated to remain set");
        };
        assert_eq!(t1, t2);

        // A failure counter bump without status change also leaves it alone.
        tracker.update_from_handshake(peer, false);
        let Some(t3) = tracker.last_updated(&peer) else {
            panic!("expected last_updated to remain set");
        };
        assert_eq!(t3, t1);
    }
}
