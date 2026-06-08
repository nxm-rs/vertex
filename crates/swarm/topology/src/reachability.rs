//! Per-peer reachability tracker.
//!
//! `Reachable` means a peer accepts *new* inbound connections at its advertised
//! address - not merely that it is alive on the connection we already hold. The
//! distinction matters: a NAT'd peer that dials us answers pings over its
//! ephemeral inbound port but is unreachable by anyone else. Only two signals
//! prove genuine reachability, and only they set `Reachable`:
//!
//! * **AutoNAT v2 dial-back**: when our node acts as an AutoNAT v2 server and
//!   successfully dials a peer back, the dial-back uses a freshly allocated port
//!   to the peer's advertised address, so it cannot be fooled by the existing
//!   connection. The node wiring forwards the confirmed peer via
//!   [`Self::on_autonat_peer_confirmed`].
//! * **Outbound dial to a public address**: when *we* dialed the peer at a
//!   public-scope address and the connection succeeded, that address is
//!   reachable. The topology behaviour forwards this via
//!   [`Self::on_outbound_reachable`].
//!
//! Ping and handshake outcomes are **liveness** signals, not reachability:
//!
//! * A successful ping or handshake clears the failure streak and recovers a
//!   peer from `Unreachable` back to `Unknown`, but never promotes to `Reachable` (it
//!   says nothing about inbound reachability, especially for peers that dialed
//!   us). See [`Self::update_from_ping`] / [`Self::update_from_handshake`].
//! * Failed pings and peer-fault handshakes accumulate; once the count crosses
//!   [`FAILURE_THRESHOLD`] within [`FAILURE_DECAY`] the peer flips to `Unreachable`.
//!   Ambiguous errors (timeouts, connection-closed-by-either-side, IO) are
//!   filtered upstream and never reach the tracker. A single negative signal
//!   never blacklists a peer.
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
/// [`PeerReachability::Unreachable`]. Picked on the lenient side so that
/// transient network blips do not blacklist peers.
pub const FAILURE_THRESHOLD: u32 = 3;

/// How long after the first counted failure the running count is allowed to
/// grow before it resets. Three failures spread over weeks no longer
/// constitute evidence; the decay window forces them to be recent.
pub const FAILURE_DECAY: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Reachability verdict for an individual peer.
///
/// This is distinct from address scope ([`vertex_net_local::AddressScope`]):
/// scope classifies an IP by its RFC range (public vs RFC-1918/ULA private),
/// whereas this enum records whether the peer accepts *new* inbound connections.
/// A peer can advertise a public-scope address yet be `Unreachable`, or sit on a
/// private LAN address yet be `Reachable` to same-subnet peers.
///
/// `Ord` ranks the verdicts from least to most worth keeping
/// (`Unreachable < Unknown < Reachable`), following declaration order. The
/// eviction logic relies on this: the minimum is dropped first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[non_exhaustive]
pub enum PeerReachability {
    /// Peer is behind NAT or otherwise does not accept new inbound connections.
    Unreachable,
    /// No reachability signal yet — treated as neutral.
    #[default]
    Unknown,
    /// Peer is confirmed to accept new inbound connections.
    Reachable,
}

impl PeerReachability {
    /// `true` if status is [`PeerReachability::Reachable`].
    pub fn is_reachable(&self) -> bool {
        matches!(self, Self::Reachable)
    }

    /// `true` if status is [`PeerReachability::Unreachable`].
    pub fn is_unreachable(&self) -> bool {
        matches!(self, Self::Unreachable)
    }
}

/// Per-peer record kept inside [`ReachabilityTracker`].
#[derive(Debug, Clone, Copy)]
struct PeerReachabilityRecord {
    status: PeerReachability,
    /// Last time the `status` *transitioned* to a different value. Re-affirming
    /// the current status (e.g. repeated handshake successes for a Reachable
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
    /// (e.g. successive handshake successes for a Reachable peer) leaves it
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
    /// [`PeerReachability::Reachable`].
    pub fn on_autonat_peer_confirmed(&self, peer: PeerId) {
        self.set_reachable(peer, "autonat");
    }

    /// Record that a peer is reachable because *we* dialed it outbound at a
    /// public-scope address and the connection succeeded.
    ///
    /// Reaching a public address proves it is dialable, so the peer is promoted
    /// to [`PeerReachability::Reachable`]. Outbound success to a private/LAN
    /// address must not be routed here (it only proves local reachability); the
    /// topology behaviour gates on the dialed address scope.
    pub fn on_outbound_reachable(&self, peer: PeerId) {
        self.set_reachable(peer, "outbound");
    }

    /// Update from a `libp2p::ping` round-trip outcome.
    ///
    /// A **liveness** signal, not a reachability one: a successful ping clears
    /// the failure streak and recovers the peer from [`PeerReachability::Unreachable`]
    /// to [`PeerReachability::Unknown`], but never promotes to
    /// [`PeerReachability::Reachable`] (a ping rides the existing connection and so
    /// proves nothing about inbound reachability). Failed pings accumulate and,
    /// once [`FAILURE_THRESHOLD`] land within [`FAILURE_DECAY`], flip the peer to
    /// [`PeerReachability::Unreachable`].
    pub fn update_from_ping(&self, peer: PeerId, success: bool) {
        self.record_liveness(peer, success, "ping");
    }

    /// Update from a handshake outcome.
    ///
    /// Shares the liveness semantics of
    /// [`update_from_ping`](Self::update_from_ping): success clears the failure
    /// streak (and recovers `Unreachable` to `Unknown`) without promoting to
    /// `Reachable`; repeated peer-fault failures demote. Genuine reachability comes
    /// only from [`Self::on_autonat_peer_confirmed`] or
    /// [`Self::on_outbound_reachable`].
    pub fn update_from_handshake(&self, peer: PeerId, success: bool) {
        self.record_liveness(peer, success, "handshake");
    }

    // ---------------------------------------------------------------------
    // Internal helpers
    // ---------------------------------------------------------------------

    fn set_reachable(&self, peer: PeerId, source: &'static str) {
        let mut guard = self.inner.write();
        let entry = guard
            .entry(peer)
            .or_insert_with(|| PeerReachabilityRecord::new(PeerReachability::Unknown));
        entry.failures = 0;
        if entry.set_status(PeerReachability::Reachable) {
            debug!(%peer, source, "peer reachability set to Reachable");
        }
    }

    /// Shared streak logic for the two liveness signals (ping, handshake).
    ///
    /// Success clears the failure run and recovers a `Unreachable` peer to
    /// `Unknown`, but never promotes to `Reachable` (liveness is not reachability;
    /// see the module docs). A failure accumulates; the run decays if its first
    /// failure predates [`FAILURE_DECAY`], and the peer flips to `Unreachable` once
    /// [`FAILURE_THRESHOLD`] consecutive failures land inside the window. A
    /// single negative signal never blacklists a peer.
    fn record_liveness(&self, peer: PeerId, success: bool, source: &'static str) {
        let mut guard = self.inner.write();
        let entry = guard
            .entry(peer)
            .or_insert_with(|| PeerReachabilityRecord::new(PeerReachability::Unknown));

        if success {
            entry.clear_failures();
            // Liveness recovers a demoted peer but does not assert reachability;
            // a Reachable verdict only comes from autonat / outbound-dial signals.
            if entry.status == PeerReachability::Unreachable
                && entry.set_status(PeerReachability::Unknown)
            {
                debug!(%peer, source, "peer reachability recovered to Unknown after liveness success");
            } else {
                trace!(%peer, source, "liveness success");
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
        if entry.failures >= FAILURE_THRESHOLD && entry.set_status(PeerReachability::Unreachable) {
            debug!(
                %peer,
                source,
                failures = entry.failures,
                "peer reachability set to Unreachable after repeated failures"
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
    fn handshake_success_is_liveness_not_public() {
        // Liveness (alive on the current connection) must not be mistaken for
        // public reachability: a peer that merely completed a handshake stays
        // Unknown until a dial-back or outbound-dial signal verifies it.
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_handshake(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Unknown);
        assert!(tracker.contains(&peer));
        assert_eq!(tracker.len(), 1);
    }

    #[test]
    fn ping_success_is_liveness_not_public() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_ping(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Unknown);
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
        assert_eq!(tracker.status(&peer), PeerReachability::Unreachable);
    }

    #[test]
    fn liveness_success_resets_failure_counter() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        for _ in 0..(FAILURE_THRESHOLD - 1) {
            tracker.update_from_handshake(peer, false);
        }
        // A success below the threshold clears the run, so a later single
        // failure must not flip to Unreachable.
        tracker.update_from_handshake(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Unknown);
        tracker.update_from_handshake(peer, false);
        assert_eq!(tracker.status(&peer), PeerReachability::Unknown);
    }

    #[test]
    fn liveness_success_recovers_private_to_unknown() {
        // A peer demoted to Unreachable that starts responding again recovers to
        // Unknown (alive but not re-verified as reachable).
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        for _ in 0..FAILURE_THRESHOLD {
            tracker.update_from_ping(peer, false);
        }
        assert_eq!(tracker.status(&peer), PeerReachability::Unreachable);
        tracker.update_from_ping(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Unknown);
    }

    #[test]
    fn liveness_success_does_not_demote_public() {
        // A verified-Reachable peer stays Reachable across ongoing liveness traffic.
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.on_autonat_peer_confirmed(peer);
        tracker.update_from_ping(peer, true);
        tracker.update_from_handshake(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Reachable);
    }

    #[test]
    fn repeated_ping_failures_flip_to_private() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        for _ in 0..FAILURE_THRESHOLD {
            tracker.update_from_ping(peer, false);
        }
        assert_eq!(tracker.status(&peer), PeerReachability::Unreachable);
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
        assert_eq!(tracker.status(&peer), PeerReachability::Unreachable);
    }

    #[test]
    fn forget_removes_record() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.on_autonat_peer_confirmed(peer);
        let dropped = tracker.forget(&peer);
        assert_eq!(dropped, Some(PeerReachability::Reachable));
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
    fn ord_ranks_reachable_above_unknown_above_unreachable() {
        // The eviction tiebreak relies on this ordering: least-reachable
        // (the Ord minimum) is evicted first.
        let tracker = ReachabilityTracker::new();
        let reachable_peer = random_peer();
        let unreachable_peer = random_peer();
        let unknown_peer = random_peer();

        tracker.on_autonat_peer_confirmed(reachable_peer);
        for _ in 0..FAILURE_THRESHOLD {
            tracker.update_from_handshake(unreachable_peer, false);
        }

        assert!(tracker.status(&reachable_peer) > tracker.status(&unknown_peer));
        assert!(tracker.status(&unknown_peer) > tracker.status(&unreachable_peer));
    }

    #[test]
    fn autonat_peer_confirmed_promotes_to_public() {
        // A successful AutoNAT v2 dial-back promotes the verified peer; the
        // node wiring forwards each such event through this entry point.
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.on_autonat_peer_confirmed(peer);
        assert_eq!(tracker.status(&peer), PeerReachability::Reachable);
    }

    #[test]
    fn outbound_reachable_promotes_to_public() {
        // Successfully dialing a peer at a public address proves reachability.
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.on_outbound_reachable(peer);
        assert_eq!(tracker.status(&peer), PeerReachability::Reachable);
    }

    #[test]
    fn autonat_peer_confirmed_clears_handshake_failures() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        for _ in 0..(FAILURE_THRESHOLD - 1) {
            tracker.update_from_handshake(peer, false);
        }
        tracker.on_autonat_peer_confirmed(peer);
        assert_eq!(tracker.status(&peer), PeerReachability::Reachable);
        // Failure counter is reset — a single further failure must not flip.
        tracker.update_from_handshake(peer, false);
        assert_eq!(tracker.status(&peer), PeerReachability::Reachable);
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
    fn ord_total_order() {
        assert!(PeerReachability::Reachable > PeerReachability::Unknown);
        assert!(PeerReachability::Unknown > PeerReachability::Unreachable);
    }

    #[test]
    fn cloned_tracker_shares_state() {
        let tracker = ReachabilityTracker::new();
        let clone = tracker.clone();
        let peer = random_peer();
        tracker.on_autonat_peer_confirmed(peer);
        assert_eq!(clone.status(&peer), PeerReachability::Reachable);
        assert_eq!(clone.len(), 1);
    }

    #[test]
    fn last_updated_changes_on_status_change_only() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.on_autonat_peer_confirmed(peer);
        let Some(t1) = tracker.last_updated(&peer) else {
            panic!("expected last_updated to be set after promotion to Reachable");
        };

        // Reaffirming Reachable does not change `last_updated`.
        std::thread::sleep(std::time::Duration::from_millis(2));
        tracker.on_autonat_peer_confirmed(peer);
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
