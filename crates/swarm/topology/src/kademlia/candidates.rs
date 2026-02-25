//! TOCTOU-safe candidate selection for peer dialing.
//!
//! Provides atomic state capture to prevent race conditions between
//! checking peer eligibility and adding to candidates.

use std::collections::{HashMap, HashSet};

use vertex_swarm_peer_manager::{PeerManager, ProximityIndex};
use vertex_swarm_primitives::OverlayAddress;

use super::limits::LimitsSnapshot;
use super::DepthAwareLimits;

/// Captured state for consistent candidate selection.
///
/// Solves TOCTOU by capturing all relevant state at a single point in time:
/// - Current depth and limits
/// - Peers already in-progress (dialing/handshaking)
/// - Peers already queued for dialing
/// - Banned peers
/// - Peers in backoff
///
/// Connected peers are checked live via `ProximityIndex::exists()` in the
/// selector rather than being snapshotted, avoiding a full iteration of all
/// bins to build a `HashSet`.
#[derive(Clone)]
pub struct CandidateSnapshot {
    /// Limits snapshot (includes depth at capture time).
    pub limits: LimitsSnapshot,
    /// Peers currently in connection phases (dialing or handshaking).
    pub in_progress: HashSet<OverlayAddress>,
    /// Peers already queued for dialing (not yet reserved).
    pub queued: HashSet<OverlayAddress>,
    /// Banned peers at snapshot time.
    pub banned: HashSet<OverlayAddress>,
    /// Peers in backoff at snapshot time.
    pub in_backoff: HashSet<OverlayAddress>,
}

impl CandidateSnapshot {
    /// Capture current state atomically.
    ///
    /// Note: This captures the state at a point in time. The captured state
    /// may become stale, but decisions made using this snapshot will be
    /// internally consistent.
    pub fn capture<F, Q>(
        limits: &DepthAwareLimits,
        depth: u8,
        peer_manager: &PeerManager,
        get_in_progress: F,
        get_queued: Q,
    ) -> Self
    where
        F: FnOnce() -> HashSet<OverlayAddress>,
        Q: FnOnce() -> HashSet<OverlayAddress>,
    {
        let limits_snapshot = LimitsSnapshot::capture(limits, depth);
        let in_progress = get_in_progress();
        let queued = get_queued();

        // Snapshot banned/backoff from DashMap (iterates PeerEntry atomics)
        let banned = peer_manager.banned_set();
        let in_backoff = peer_manager.peers_in_backoff();

        Self {
            limits: limits_snapshot,
            in_progress,
            queued,
            banned,
            in_backoff,
        }
    }

    /// Lightweight capture that only snapshots limits and in-progress.
    ///
    /// Use when you'll check ban/backoff status individually (acceptable
    /// for small candidate sets where O(1) lookups are fine).
    pub fn capture_lightweight<F>(limits: &DepthAwareLimits, depth: u8, get_in_progress: F) -> Self
    where
        F: FnOnce() -> HashSet<OverlayAddress>,
    {
        Self {
            limits: LimitsSnapshot::capture(limits, depth),
            in_progress: get_in_progress(),
            queued: HashSet::new(),
            banned: HashSet::new(),
            in_backoff: HashSet::new(),
        }
    }

    /// Check if peer is eligible for dialing based on snapshot.
    ///
    /// Returns true if:
    /// - Not in-progress (dialing/handshaking)
    /// - Not already queued for dialing
    /// - Not banned (in snapshot)
    /// - Not in backoff (in snapshot)
    ///
    /// Note: Connected peer check is performed by `CandidateSelector` via
    /// live `ProximityIndex::exists()` lookups.
    pub fn is_eligible(&self, peer: &OverlayAddress) -> bool {
        !self.in_progress.contains(peer)
            && !self.queued.contains(peer)
            && !self.banned.contains(peer)
            && !self.in_backoff.contains(peer)
    }

    /// Check eligibility with live ban/backoff check.
    ///
    /// Use when snapshot was created with `capture_lightweight`.
    pub fn is_eligible_live(&self, peer: &OverlayAddress, peer_manager: &PeerManager) -> bool {
        !self.in_progress.contains(peer)
            && !self.queued.contains(peer)
            && !peer_manager.is_banned(peer)
            && !peer_manager.peer_is_in_backoff(peer)
    }

    /// Current depth from snapshot.
    pub fn depth(&self) -> u8 {
        self.limits.depth
    }

    /// Check if bin needs more peers.
    pub fn needs_more(&self, bin: u8, connected: usize) -> bool {
        self.limits.needs_more(bin, connected)
    }

    /// Get deficit for bin.
    pub fn deficit(&self, bin: u8, connected: usize) -> usize {
        self.limits.deficit(bin, connected)
    }

    /// Check if bin is in neighborhood.
    pub fn is_neighborhood(&self, bin: u8) -> bool {
        self.limits.is_neighborhood(bin)
    }
}

/// Builder for selecting dial candidates with TOCTOU safety and per-bin capacity tracking.
///
/// Borrows a `CandidateSnapshot` and a `ProximityIndex` (for live connected-peer
/// checks) to avoid cloning the snapshot and building a full `HashSet` of
/// connected peers each tick.
pub struct CandidateSelector<'a> {
    snapshot: &'a CandidateSnapshot,
    connected_peers: &'a ProximityIndex,
    candidates: Vec<OverlayAddress>,
    max_candidates: usize,
    /// Per-bin selection counts to enforce capacity limits.
    bin_selections: HashMap<u8, usize>,
}

impl<'a> CandidateSelector<'a> {
    /// Create a new selector with borrowed snapshot and connected-peer index.
    pub fn new(
        snapshot: &'a CandidateSnapshot,
        connected_peers: &'a ProximityIndex,
        max_candidates: usize,
    ) -> Self {
        Self {
            snapshot,
            connected_peers,
            candidates: Vec::with_capacity(max_candidates),
            max_candidates,
            bin_selections: HashMap::new(),
        }
    }

    /// Current number of selected candidates.
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Check if we've reached max candidates.
    pub fn is_full(&self) -> bool {
        self.candidates.len() >= self.max_candidates
    }

    /// Remaining capacity.
    pub fn remaining(&self) -> usize {
        self.max_candidates.saturating_sub(self.candidates.len())
    }

    /// Get count of candidates already selected for a bin.
    pub fn bin_selected(&self, bin: u8) -> usize {
        self.bin_selections.get(&bin).copied().unwrap_or(0)
    }

    /// Try to add a peer as candidate.
    ///
    /// Returns true if added, false if ineligible or at capacity.
    pub fn try_add(&mut self, peer: OverlayAddress) -> bool {
        if self.is_full() {
            return false;
        }

        if self.connected_peers.exists(&peer) {
            return false;
        }

        if !self.snapshot.is_eligible(&peer) {
            return false;
        }

        // Prevent duplicates
        if self.candidates.contains(&peer) {
            return false;
        }

        self.candidates.push(peer);
        true
    }

    /// Try to add with live eligibility check and bin capacity enforcement.
    pub fn try_add_live(&mut self, peer: OverlayAddress, peer_manager: &PeerManager) -> bool {
        if self.is_full() {
            return false;
        }

        if self.connected_peers.exists(&peer) {
            return false;
        }

        if !self.snapshot.is_eligible_live(&peer, peer_manager) {
            return false;
        }

        if self.candidates.contains(&peer) {
            return false;
        }

        self.candidates.push(peer);
        true
    }

    /// Try to add with bin capacity enforcement.
    ///
    /// Takes the peer's bin and current effective count for that bin.
    /// Only adds if the bin still has capacity after accounting for already-selected candidates.
    pub fn try_add_with_bin_capacity(
        &mut self,
        peer: OverlayAddress,
        bin: u8,
        effective_count: usize,
        peer_manager: &PeerManager,
    ) -> bool {
        if self.is_full() {
            return false;
        }

        if self.connected_peers.exists(&peer) {
            return false;
        }

        if !self.snapshot.is_eligible_live(&peer, peer_manager) {
            return false;
        }

        if self.candidates.contains(&peer) {
            return false;
        }

        // Check bin capacity: effective + already_selected < target
        let already_selected = self.bin_selected(bin);
        let projected_count = effective_count + already_selected;

        if !self.snapshot.needs_more(bin, projected_count) {
            return false;
        }

        self.candidates.push(peer);
        *self.bin_selections.entry(bin).or_insert(0) += 1;
        true
    }

    /// Access the snapshot.
    pub fn snapshot(&self) -> &CandidateSnapshot {
        self.snapshot
    }

    /// Consume and return selected candidates.
    pub fn finish(self) -> Vec<OverlayAddress> {
        self.candidates
    }
}

/// Select candidates for neighborhood bins (high PO, >= depth).
///
/// Prioritizes bins from highest to depth, connecting to all available.
/// Enforces per-bin capacity to prevent selecting more candidates than can be dialed.
pub fn select_neighborhood_candidates(
    selector: &mut CandidateSelector<'_>,
    peer_manager: &PeerManager,
    connected_counts: impl Fn(u8) -> usize,
    max_po: u8,
) {
    let depth = selector.snapshot().depth();

    // Iterate from highest PO down to depth
    for po in (depth..=max_po).rev() {
        if selector.is_full() {
            break;
        }

        let effective = connected_counts(po);
        let already_selected = selector.bin_selected(po);

        // Check if bin still needs more (accounting for pending selections)
        if !selector.snapshot().needs_more(po, effective + already_selected) {
            continue;
        }

        // Get peers from this bin (LRU order via KnownPeers)
        for peer in peer_manager.dialable_overlays_in_bin(po, selector.remaining()) {
            if !selector.try_add_with_bin_capacity(peer, po, effective, peer_manager) {
                continue;
            }
        }
    }
}

/// Select candidates for balanced bins (lower PO, < depth).
///
/// Uses linear tapering - higher bins get more allocation.
/// Enforces per-bin capacity to prevent selecting more candidates than can be dialed.
pub fn select_balanced_candidates(
    selector: &mut CandidateSelector<'_>,
    peer_manager: &PeerManager,
    connected_counts: impl Fn(u8) -> usize,
) {
    let depth = selector.snapshot().depth();
    if depth == 0 {
        return;
    }

    // Collect bin stats: (po, effective, deficit)
    let mut bin_stats: Vec<(u8, usize, usize)> = Vec::new();

    for po in 0..depth {
        let effective = connected_counts(po);
        let already_selected = selector.bin_selected(po);

        // Account for pending selections when checking
        if !selector.snapshot().needs_more(po, effective + already_selected) {
            continue;
        }

        let deficit = selector.snapshot().deficit(po, effective + already_selected);
        if deficit > 0 {
            bin_stats.push((po, effective, deficit));
        }
    }

    // Sort by PO descending (prioritize higher bins)
    bin_stats.sort_by(|a, b| b.0.cmp(&a.0));

    for (po, effective, deficit) in bin_stats {
        if selector.is_full() {
            break;
        }

        // Limit to remaining deficit for this bin
        let to_add = deficit.min(selector.remaining());
        let mut added = 0;

        for peer in peer_manager.dialable_overlays_in_bin(po, to_add) {
            if added >= to_add || selector.is_full() {
                break;
            }
            if selector.try_add_with_bin_capacity(peer, po, effective, peer_manager) {
                added += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_overlay(n: u8) -> OverlayAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        OverlayAddress::from(bytes)
    }

    fn test_proximity_index() -> ProximityIndex {
        ProximityIndex::new(OverlayAddress::from([0u8; 32]), 31, 0)
    }

    #[test]
    fn test_snapshot_eligibility() {
        let mut in_progress = HashSet::new();
        in_progress.insert(test_overlay(1));

        let mut queued = HashSet::new();
        queued.insert(test_overlay(5));

        let mut banned = HashSet::new();
        banned.insert(test_overlay(2));

        let mut in_backoff = HashSet::new();
        in_backoff.insert(test_overlay(3));

        let limits = DepthAwareLimits::new(160, 3);

        let snapshot = CandidateSnapshot {
            limits: LimitsSnapshot::capture(&limits, 8),
            in_progress,
            queued,
            banned,
            in_backoff,
        };

        // Eligible peer
        assert!(snapshot.is_eligible(&test_overlay(6)));

        // In-progress
        assert!(!snapshot.is_eligible(&test_overlay(1)));

        // Banned
        assert!(!snapshot.is_eligible(&test_overlay(2)));

        // In backoff
        assert!(!snapshot.is_eligible(&test_overlay(3)));

        // Queued
        assert!(!snapshot.is_eligible(&test_overlay(5)));
    }

    #[test]
    fn test_connected_peer_rejected_by_selector() {
        let limits = DepthAwareLimits::new(160, 3);
        let snapshot = CandidateSnapshot {
            limits: LimitsSnapshot::capture(&limits, 0),
            in_progress: HashSet::new(),
            queued: HashSet::new(),
            banned: HashSet::new(),
            in_backoff: HashSet::new(),
        };

        let connected = test_proximity_index();
        let peer = test_overlay(4);
        connected.add(peer);

        let mut selector = CandidateSelector::new(&snapshot, &connected, 10);

        // Connected peer should be rejected
        assert!(!selector.try_add(peer));
        assert_eq!(selector.len(), 0);

        // Non-connected peer should be accepted
        assert!(selector.try_add(test_overlay(6)));
        assert_eq!(selector.len(), 1);
    }

    #[test]
    fn test_selector_prevents_duplicates() {
        let limits = DepthAwareLimits::new(160, 3);
        let snapshot = CandidateSnapshot {
            limits: LimitsSnapshot::capture(&limits, 0),
            in_progress: HashSet::new(),
            queued: HashSet::new(),
            banned: HashSet::new(),
            in_backoff: HashSet::new(),
        };

        let connected = test_proximity_index();
        let mut selector = CandidateSelector::new(&snapshot, &connected, 10);

        let peer = test_overlay(1);
        assert!(selector.try_add(peer));
        assert!(!selector.try_add(peer)); // Duplicate

        assert_eq!(selector.len(), 1);
    }

    #[test]
    fn test_selector_respects_max() {
        let limits = DepthAwareLimits::new(160, 3);
        let snapshot = CandidateSnapshot {
            limits: LimitsSnapshot::capture(&limits, 0),
            in_progress: HashSet::new(),
            queued: HashSet::new(),
            banned: HashSet::new(),
            in_backoff: HashSet::new(),
        };

        let connected = test_proximity_index();
        let mut selector = CandidateSelector::new(&snapshot, &connected, 2);

        assert!(selector.try_add(test_overlay(1)));
        assert!(selector.try_add(test_overlay(2)));
        assert!(!selector.try_add(test_overlay(3))); // At capacity

        assert!(selector.is_full());
        assert_eq!(selector.remaining(), 0);
    }

    #[test]
    fn test_bin_selection_tracking() {
        let limits = DepthAwareLimits::new(160, 3);
        let snapshot = CandidateSnapshot {
            limits: LimitsSnapshot::capture(&limits, 0),
            in_progress: HashSet::new(),
            queued: HashSet::new(),
            banned: HashSet::new(),
            in_backoff: HashSet::new(),
        };

        let connected = test_proximity_index();
        let mut selector = CandidateSelector::new(&snapshot, &connected, 10);

        assert_eq!(selector.bin_selected(0), 0);
        assert_eq!(selector.bin_selected(5), 0);

        // After adding peers, bin counts should update
        // Note: try_add doesn't track bin (use try_add_with_bin_capacity for that)
        selector.try_add(test_overlay(1));
        assert_eq!(selector.bin_selected(0), 0); // try_add doesn't track bins

        // We'd need a PeerManager to test try_add_with_bin_capacity
    }
}
