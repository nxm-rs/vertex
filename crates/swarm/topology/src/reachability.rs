//! Per-peer reachability tracker.
//!
//! Three independent signals feed a single per-peer verdict that the kademlia
//! routing layer consumes when picking eviction candidates and counting bin
//! saturation:
//!
//! * **AutoNAT probe responses** promote a peer: a successful outbound or
//!   inbound probe proves the remote is reachable end-to-end. Probe failures
//!   do not demote, because the failure may be on our side.
//! * **Handshake outcomes** promote on success and accumulate consecutive
//!   protocol-fault failures; once the count crosses
//!   [`HANDSHAKE_FAILURE_THRESHOLD`] within
//!   [`HANDSHAKE_FAILURE_DECAY`], the peer flips to `Private`. The decay
//!   window prevents historical failures from outweighing recent evidence.
//!   Ambiguous errors (timeouts, connection-closed-by-either-side, IO) are
//!   ignored upstream and never reach the tracker.
//! * **Stabilization** locks a peer to `Public` while pingpong RTT is steady.
//!   Loss of stability demotes only peers that were promoted via
//!   stabilization in the first place, so a spurious negative cannot
//!   blacklist a peer reached via handshake or AutoNAT.
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

/// Number of consecutive handshake protocol-fault failures within
/// [`HANDSHAKE_FAILURE_DECAY`] that flip a peer to
/// [`PeerReachability::Private`]. Picked on the lenient side so that
/// transient network blips do not blacklist peers.
pub const HANDSHAKE_FAILURE_THRESHOLD: u32 = 3;

/// How long after the first counted failure the running count is allowed to
/// grow before it resets. Three failures spread over weeks no longer
/// constitute evidence; the decay window forces them to be recent.
pub const HANDSHAKE_FAILURE_DECAY: std::time::Duration = std::time::Duration::from_secs(5 * 60);

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

    /// Total order used by Unit 8's eviction policy: `Public` survives,
    /// `Unknown` is neutral, `Private` is evicted first. Higher value = better.
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
    /// Consecutive handshake failures since the last success or decay window
    /// reset.
    handshake_failures: u32,
    /// Time of the first failure in the current run. Used to decay the
    /// counter once [`HANDSHAKE_FAILURE_DECAY`] has elapsed without crossing
    /// the threshold.
    first_failure_at: Option<Instant>,
    /// `true` if stabilization has promoted this peer to `Public`. Sticky:
    /// handshake signals will not demote a stable peer until
    /// `update_from_stabilization(peer, false)` flips it back to `Private`.
    stable: bool,
}

impl PeerReachabilityRecord {
    fn new(status: PeerReachability) -> Self {
        Self {
            status,
            last_updated: Instant::now(),
            handshake_failures: 0,
            first_failure_at: None,
            stable: false,
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
        self.handshake_failures = 0;
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

    /// Update from an AutoNAT outbound or inbound probe outcome.
    ///
    /// `OutboundProbe::Response` (a remote server successfully dialed us back)
    /// and `InboundProbe::Response` (we successfully dialed a peer that asked
    /// for a dial-back) both prove the remote peer is publicly reachable —
    /// otherwise the probe round-trip could not have completed. We promote in
    /// both cases.
    ///
    /// `StatusChanged` is about *our* reachability and is consumed by
    /// `LocalAddressManager::on_observed_addr`; this method ignores it.
    pub fn update_from_autonat(&self, event: &libp2p::autonat::Event) {
        match event {
            libp2p::autonat::Event::OutboundProbe(
                libp2p::autonat::OutboundProbeEvent::Response { peer, .. },
            ) => {
                self.on_autonat_peer_confirmed(*peer);
            }
            libp2p::autonat::Event::InboundProbe(
                libp2p::autonat::InboundProbeEvent::Response { peer, .. },
            ) => {
                self.on_autonat_peer_confirmed(*peer);
            }
            _ => {}
        }
    }

    /// Record that a peer has been confirmed reachable via an AutoNAT probe.
    ///
    /// Direct entry point used by AutoNAT wiring and by tests that cannot
    /// easily construct a full `libp2p::autonat::Event` (its `ProbeId`
    /// constructor is crate-private).
    pub fn on_autonat_peer_confirmed(&self, peer: PeerId) {
        self.set_public(peer, "autonat");
    }

    /// Update from a handshake completion.
    ///
    /// Successful handshakes promote the peer to [`PeerReachability::Public`]
    /// (unless stabilization-locked, in which case the signal is ignored as a
    /// no-op confirmation). Failures accumulate; once
    /// [`HANDSHAKE_FAILURE_THRESHOLD`] consecutive failures have been observed
    /// the peer flips to [`PeerReachability::Private`].
    pub fn update_from_handshake(&self, peer: PeerId, success: bool) {
        let mut guard = self.inner.write();
        let entry = guard
            .entry(peer)
            .or_insert_with(|| PeerReachabilityRecord::new(PeerReachability::Unknown));

        if entry.stable {
            // Stabilization-locked peers ignore handshake signals; only an
            // explicit `update_from_stabilization(peer, false)` can demote them.
            return;
        }

        if success {
            entry.clear_failures();
            if entry.set_status(PeerReachability::Public) {
                debug!(%peer, "peer reachability set to Public via handshake");
            } else {
                trace!(%peer, "handshake success reaffirms Public");
            }
        } else {
            let now = Instant::now();
            // Decay the run if the first failure is too far in the past; we
            // do not want stale historical failures crossing the threshold.
            if let Some(start) = entry.first_failure_at
                && now.duration_since(start) > HANDSHAKE_FAILURE_DECAY
            {
                entry.clear_failures();
            }
            if entry.first_failure_at.is_none() {
                entry.first_failure_at = Some(now);
            }
            entry.handshake_failures = entry.handshake_failures.saturating_add(1);
            if entry.handshake_failures >= HANDSHAKE_FAILURE_THRESHOLD
                && entry.set_status(PeerReachability::Private)
            {
                debug!(
                    %peer,
                    failures = entry.handshake_failures,
                    "peer reachability set to Private after repeated handshake failures"
                );
            } else {
                trace!(
                    %peer,
                    failures = entry.handshake_failures,
                    "handshake failure noted"
                );
            }
        }
    }

    /// Update from the stabilization detector.
    ///
    /// * `stable = true` locks the peer to [`PeerReachability::Public`];
    ///   handshake failures will not demote it. Creates a record if the peer
    ///   was previously untracked.
    /// * `stable = false` releases the lock and demotes the peer to
    ///   [`PeerReachability::Private`] **only if it was actually
    ///   stabilization-promoted in the first place**. Peers promoted by
    ///   handshake or AutoNAT are left alone, and untracked peers are
    ///   ignored. This preserves the invariant that a spurious negative
    ///   signal cannot blacklist a peer.
    pub fn update_from_stabilization(&self, peer: PeerId, stable: bool) {
        let mut guard = self.inner.write();

        if stable {
            let entry = guard
                .entry(peer)
                .or_insert_with(|| PeerReachabilityRecord::new(PeerReachability::Unknown));
            entry.stable = true;
            entry.clear_failures();
            if entry.set_status(PeerReachability::Public) {
                debug!(%peer, "peer reachability locked to Public via stabilization");
            }
        } else if let Some(entry) = guard.get_mut(&peer) {
            if !entry.stable {
                trace!(
                    %peer,
                    "stabilization loss for non-stabilized peer ignored"
                );
                return;
            }
            entry.stable = false;
            if entry.set_status(PeerReachability::Private) {
                debug!(%peer, "peer reachability set to Private via stabilization loss");
            }
        } else {
            trace!(%peer, "stabilization loss for untracked peer ignored");
        }
    }

    // ---------------------------------------------------------------------
    // Consumer surface for Unit 8 (Kademlia eviction / saturation accounting)
    // ---------------------------------------------------------------------

    /// Predicate used by the kademlia routing layer to exclude private peers
    /// from saturation counts in non-neighborhood bins. Returns `true` if
    /// `peer` should count toward bin capacity.
    pub fn counts_toward_saturation(&self, peer: &PeerId) -> bool {
        !self.status(peer).is_private()
    }

    /// Comparator used by Unit 8's eviction policy: returns the **survivor**
    /// of two peers, preferring [`PeerReachability::Public`]. Ties fall back
    /// to the first argument; the caller can break further ties on score or
    /// `last_seen`.
    pub fn prefer_survivor<'a>(&self, a: &'a PeerId, b: &'a PeerId) -> &'a PeerId {
        let rank_a = self.status(a).rank();
        let rank_b = self.status(b).rank();
        if rank_b > rank_a { b } else { a }
    }

    // ---------------------------------------------------------------------
    // Internal helpers
    // ---------------------------------------------------------------------

    fn set_public(&self, peer: PeerId, source: &'static str) {
        let mut guard = self.inner.write();
        let entry = guard
            .entry(peer)
            .or_insert_with(|| PeerReachabilityRecord::new(PeerReachability::Unknown));
        entry.handshake_failures = 0;
        if entry.set_status(PeerReachability::Public) {
            debug!(%peer, source, "peer reachability set to Public");
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
        for _ in 0..HANDSHAKE_FAILURE_THRESHOLD {
            tracker.update_from_handshake(peer, false);
        }
        assert_eq!(tracker.status(&peer), PeerReachability::Private);
    }

    #[test]
    fn handshake_success_resets_failure_counter() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        for _ in 0..(HANDSHAKE_FAILURE_THRESHOLD - 1) {
            tracker.update_from_handshake(peer, false);
        }
        tracker.update_from_handshake(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
        // Now a single failure must NOT flip back to Private.
        tracker.update_from_handshake(peer, false);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
    }

    #[test]
    fn stabilization_locks_to_public() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_stabilization(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);

        // Stable peers ignore handshake failures regardless of count.
        for _ in 0..HANDSHAKE_FAILURE_THRESHOLD * 2 {
            tracker.update_from_handshake(peer, false);
        }
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
    }

    #[test]
    fn stabilization_loss_flips_to_private_and_releases_lock() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_stabilization(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);

        tracker.update_from_stabilization(peer, false);
        assert_eq!(tracker.status(&peer), PeerReachability::Private);

        // After demotion, a handshake success can promote again (lock released).
        tracker.update_from_handshake(peer, true);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
    }

    #[test]
    fn stabilization_loss_on_untracked_peer_is_noop() {
        // A spurious `stable = false` for a peer we've never seen must not
        // synthesise a `Private` record from nothing.
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_stabilization(peer, false);
        assert!(!tracker.contains(&peer));
        assert_eq!(tracker.status(&peer), PeerReachability::Unknown);
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
    fn counts_toward_saturation_excludes_private() {
        let tracker = ReachabilityTracker::new();
        let public_peer = random_peer();
        let private_peer = random_peer();
        let unknown_peer = random_peer();

        tracker.update_from_handshake(public_peer, true);
        for _ in 0..HANDSHAKE_FAILURE_THRESHOLD {
            tracker.update_from_handshake(private_peer, false);
        }

        assert!(tracker.counts_toward_saturation(&public_peer));
        assert!(tracker.counts_toward_saturation(&unknown_peer));
        assert!(!tracker.counts_toward_saturation(&private_peer));
    }

    #[test]
    fn prefer_survivor_picks_public() {
        let tracker = ReachabilityTracker::new();
        let public_peer = random_peer();
        let private_peer = random_peer();
        let unknown_peer = random_peer();

        tracker.update_from_handshake(public_peer, true);
        for _ in 0..HANDSHAKE_FAILURE_THRESHOLD {
            tracker.update_from_handshake(private_peer, false);
        }

        // Public beats Private regardless of argument order.
        assert_eq!(
            tracker.prefer_survivor(&public_peer, &private_peer),
            &public_peer
        );
        assert_eq!(
            tracker.prefer_survivor(&private_peer, &public_peer),
            &public_peer
        );

        // Unknown beats Private.
        assert_eq!(
            tracker.prefer_survivor(&unknown_peer, &private_peer),
            &unknown_peer
        );

        // Equal status: first argument wins (caller breaks the tie elsewhere).
        let other_public = random_peer();
        tracker.update_from_handshake(other_public, true);
        assert_eq!(
            tracker.prefer_survivor(&public_peer, &other_public),
            &public_peer
        );
    }

    #[test]
    fn autonat_peer_confirmed_promotes_to_public() {
        // `update_from_autonat` delegates to `on_autonat_peer_confirmed`; we
        // exercise the public-facing direct entry point that doesn't require
        // building an internal AutoNAT `Event` (whose `ProbeId` constructor
        // is crate-private).
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.on_autonat_peer_confirmed(peer);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
    }

    #[test]
    fn autonat_peer_confirmed_clears_handshake_failures() {
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        for _ in 0..(HANDSHAKE_FAILURE_THRESHOLD - 1) {
            tracker.update_from_handshake(peer, false);
        }
        tracker.on_autonat_peer_confirmed(peer);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
        // Failure counter is reset — a single further failure must not flip.
        tracker.update_from_handshake(peer, false);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
    }

    #[test]
    fn handshake_promoted_peer_immune_to_stability_loss() {
        // A peer made Public via handshake but never via stabilization must
        // not be demoted by a spurious `stable = false`. Otherwise the
        // invariant "a single negative signal cannot blacklist a peer" is
        // broken.
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_handshake(peer, true);
        tracker.update_from_stabilization(peer, false);
        assert_eq!(tracker.status(&peer), PeerReachability::Public);
    }

    #[test]
    fn failure_counter_decays_after_window() {
        // Three failures spaced over a long window must not cumulatively
        // cross the threshold; only failures within HANDSHAKE_FAILURE_DECAY
        // of each other count.
        let tracker = ReachabilityTracker::new();
        let peer = random_peer();
        tracker.update_from_handshake(peer, false);

        // Backdate the record's first failure so the next call sees it
        // outside the decay window.
        {
            let mut guard = tracker.inner.write();
            let entry = guard.get_mut(&peer).expect("entry exists after failure");
            entry.first_failure_at = Some(Instant::now() - HANDSHAKE_FAILURE_DECAY * 2);
        }

        for _ in 0..(HANDSHAKE_FAILURE_THRESHOLD - 1) {
            tracker.update_from_handshake(peer, false);
        }
        // The first failure decayed; only HANDSHAKE_FAILURE_THRESHOLD - 1
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
