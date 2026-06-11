//! TOCTOU-safe candidate selection for peer dialing.

use std::collections::{HashMap, HashSet};

use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer_manager::{PeerManager, ProximityIndex};
use vertex_swarm_primitives::{
    Bin, NeighborhoodDepth, OverlayAddress, balanced_bins, neighborhood_bins,
};

use super::limits::LimitsSnapshot;

/// Captured state for consistent candidate selection.
///
/// Ban/backoff status is checked live via `PeerManager` rather than snapshotted,
/// keeping this struct lightweight.
pub(crate) struct CandidateSnapshot {
    /// Limits snapshot (includes depth at capture time).
    pub(crate) limits: LimitsSnapshot,
    /// Peers currently in connection phases (dialing or handshaking).
    pub(crate) in_progress: HashSet<OverlayAddress>,
    /// Peers already queued for dialing (not yet reserved).
    pub(crate) queued: HashSet<OverlayAddress>,
}

impl CandidateSnapshot {
    /// Check if peer is eligible for dialing (not in-progress, queued, banned, or in backoff).
    ///
    /// Ban/backoff status is checked live via `PeerManager`.
    pub(crate) fn is_eligible<I: SwarmIdentity>(
        &self,
        peer: &OverlayAddress,
        peer_manager: &PeerManager<I>,
    ) -> bool {
        !self.in_progress.contains(peer)
            && !self.queued.contains(peer)
            && !peer_manager.is_banned(peer)
            && !peer_manager.peer_is_in_backoff(peer)
    }
}

/// Builder for selecting dial candidates with TOCTOU safety and per-bin capacity tracking.
pub(crate) struct CandidateSelector<'a> {
    snapshot: &'a CandidateSnapshot,
    connected_peers: &'a ProximityIndex,
    candidates: Vec<OverlayAddress>,
    max_candidates: usize,
    /// Per-bin selection counts to enforce capacity limits.
    bin_selections: HashMap<Bin, usize>,
}

impl<'a> CandidateSelector<'a> {
    /// Create a new selector with borrowed snapshot and connected-peer index.
    pub(crate) fn new(
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
    pub(crate) fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Check if we've reached max candidates.
    pub(crate) fn is_full(&self) -> bool {
        self.candidates.len() >= self.max_candidates
    }

    /// Remaining capacity.
    pub(crate) fn remaining(&self) -> usize {
        self.max_candidates.saturating_sub(self.candidates.len())
    }

    /// Get count of candidates already selected for a bin.
    pub(crate) fn bin_selected(&self, bin: Bin) -> usize {
        self.bin_selections.get(&bin).copied().unwrap_or(0)
    }

    /// The connected-peer index, with the borrowed data's lifetime so callers
    /// can hold it across mutable selector use.
    pub(crate) fn connected_index(&self) -> &'a ProximityIndex {
        self.connected_peers
    }

    /// Try to add a peer as candidate (test-only, no ban/backoff check).
    ///
    /// Returns true if added, false if ineligible or at capacity.
    #[cfg(test)]
    pub(crate) fn try_add(&mut self, peer: OverlayAddress) -> bool {
        if self.is_full() {
            return false;
        }

        if self.connected_peers.exists(&peer) {
            return false;
        }

        if self.snapshot.in_progress.contains(&peer) || self.snapshot.queued.contains(&peer) {
            return false;
        }

        // Prevent duplicates
        if self.candidates.contains(&peer) {
            return false;
        }

        self.candidates.push(peer);
        true
    }

    /// Try to add with bin capacity enforcement.
    pub(crate) fn try_add_with_bin_capacity<I: SwarmIdentity>(
        &mut self,
        peer: OverlayAddress,
        bin: Bin,
        effective_count: usize,
        peer_manager: &PeerManager<I>,
    ) -> bool {
        if self.is_full() {
            return false;
        }

        if self.connected_peers.exists(&peer) {
            return false;
        }

        if !self.snapshot.is_eligible(&peer, peer_manager) {
            return false;
        }

        if self.candidates.contains(&peer) {
            return false;
        }

        // Check bin capacity: effective + already_selected < target
        let already_selected = self.bin_selected(bin);
        let projected_count = effective_count + already_selected;

        if !self.snapshot.limits.needs_more(bin, projected_count) {
            return false;
        }

        self.candidates.push(peer);
        *self.bin_selections.entry(bin).or_insert(0) += 1;
        true
    }

    /// Access the snapshot, with the borrowed data's lifetime so callers can
    /// hold it across mutable selector use.
    pub(crate) fn snapshot(&self) -> &'a CandidateSnapshot {
        self.snapshot
    }

    /// Consume and return selected candidates.
    pub(crate) fn finish(self) -> Vec<OverlayAddress> {
        self.candidates
    }
}

/// Select candidates for neighborhood bins (>= depth), highest PO first.
pub(crate) fn select_neighborhood_candidates<I: SwarmIdentity>(
    selector: &mut CandidateSelector<'_>,
    peer_manager: &PeerManager<I>,
    connected_counts: impl Fn(Bin) -> usize,
    max_bin: Bin,
) {
    let depth = selector.snapshot().limits.depth;
    // Connected peers are dialable but useless as candidates; without the
    // exclusion a bin with more connections than the requested count yields
    // only already-connected overlays and starves.
    let connected = selector.connected_index();

    // Iterate from highest PO down to depth
    for bin in neighborhood_bins(depth, max_bin).rev() {
        if selector.is_full() {
            break;
        }

        let effective = connected_counts(bin);
        let already_selected = selector.bin_selected(bin);

        if !selector
            .snapshot()
            .limits
            .needs_more(bin, effective + already_selected)
        {
            continue;
        }

        // Get peers from this bin (LRU order via KnownPeers)
        for peer in
            peer_manager.dialable_overlays_in_bin_excluding(bin, selector.remaining(), |overlay| {
                connected.exists(overlay)
            })
        {
            if !selector.try_add_with_bin_capacity(peer, bin, effective, peer_manager) {
                continue;
            }
        }
    }
}

/// Select candidates for balanced bins (< depth) using linear tapering.
pub(crate) fn select_balanced_candidates<I: SwarmIdentity>(
    selector: &mut CandidateSelector<'_>,
    peer_manager: &PeerManager<I>,
    connected_counts: impl Fn(Bin) -> usize,
) {
    let depth = selector.snapshot().limits.depth;
    if depth == NeighborhoodDepth::ZERO {
        return;
    }

    // Collect bin stats: (bin, effective, deficit)
    let mut bin_stats: Vec<(Bin, usize, usize)> = Vec::new();

    for bin in balanced_bins(depth) {
        let effective = connected_counts(bin);
        let already_selected = selector.bin_selected(bin);

        if !selector
            .snapshot()
            .limits
            .needs_more(bin, effective + already_selected)
        {
            continue;
        }

        let deficit = selector
            .snapshot()
            .limits
            .deficit(bin, effective + already_selected);
        if deficit > 0 {
            bin_stats.push((bin, effective, deficit));
        }
    }

    // Sort by PO descending (prioritize higher bins)
    bin_stats.sort_by_key(|b| std::cmp::Reverse(b.0));

    // See select_neighborhood_candidates: exclude connected peers from the
    // candidate supply or saturating bins starve their own refill.
    let connected = selector.connected_index();

    for (bin, effective, deficit) in bin_stats {
        if selector.is_full() {
            break;
        }

        // Limit to remaining deficit for this bin
        let to_add = deficit.min(selector.remaining());
        let mut added = 0;

        for peer in peer_manager
            .dialable_overlays_in_bin_excluding(bin, to_add, |overlay| connected.exists(overlay))
        {
            if added >= to_add || selector.is_full() {
                break;
            }
            if selector.try_add_with_bin_capacity(peer, bin, effective, peer_manager) {
                added += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::DepthAwareLimits;
    use super::*;
    use vertex_swarm_peer_manager::PeerManagerConfig;
    use vertex_swarm_test_utils::make_overlay;

    fn b(n: u8) -> Bin {
        Bin::new(n).expect("valid bin")
    }

    fn d(n: u8) -> NeighborhoodDepth {
        NeighborhoodDepth::new(b(n))
    }

    fn test_proximity_index() -> ProximityIndex {
        ProximityIndex::new(OverlayAddress::from([0u8; 32]), 31, 0)
    }

    #[test]
    fn test_balanced_selection_skips_connected_supply() {
        use vertex_swarm_test_utils::{MockIdentity, make_swarm_peer_minimal};

        // Regression: connected peers are hot and dialable, so without the
        // exclusion they fill the candidate slice first and a bin with more
        // connections than its deficit yields no usable candidates, starving
        // refill (observed live: every bin froze once connected >= deficit).
        let identity = MockIdentity::with_first_byte(0x00);
        let peer_manager = PeerManager::new(&identity, PeerManagerConfig::default());

        // Seven known bin-0 peers; the first five are already connected.
        let overlays: Vec<OverlayAddress> = (0..7u8).map(|i| make_overlay(0x80 + i)).collect();
        for i in 0..7u8 {
            peer_manager.store_discovered_peer(make_swarm_peer_minimal(0x80 + i));
        }
        let connected_index = test_proximity_index();
        for overlay in overlays.iter().take(5) {
            connected_index.add(*overlay).expect("add connected");
        }

        let limits = DepthAwareLimits::new(160, 3).with_saturation(8);
        let snapshot = CandidateSnapshot {
            limits: LimitsSnapshot::capture(&limits, d(8)),
            in_progress: HashSet::new(),
            queued: HashSet::new(),
        };
        let mut selector = CandidateSelector::new(&snapshot, &connected_index, 32);

        // Bin 0 holds 5 connected against a saturation-floored target of 8:
        // deficit 3, and exactly two unconnected candidates are known.
        select_balanced_candidates(&mut selector, &peer_manager, |bin| {
            if bin == b(0) { 5 } else { 0 }
        });

        let candidates = selector.finish();
        assert_eq!(
            candidates.len(),
            2,
            "the unconnected peers must be selected despite connected > deficit"
        );
        for c in &candidates {
            assert!(
                overlays.iter().skip(5).any(|o| o == c),
                "selected a connected peer"
            );
        }
    }

    #[test]
    fn test_snapshot_eligibility() {
        use vertex_swarm_test_utils::{MockIdentity, make_swarm_peer_minimal};

        let identity = MockIdentity::with_first_byte(0x00);
        let peer_manager = PeerManager::new(&identity, PeerManagerConfig::default());

        let mut in_progress = HashSet::new();
        in_progress.insert(make_overlay(1));

        let mut queued = HashSet::new();
        queued.insert(make_overlay(5));

        let limits = DepthAwareLimits::new(160, 3);

        let snapshot = CandidateSnapshot {
            limits: LimitsSnapshot::capture(&limits, d(8)),
            in_progress,
            queued,
        };

        // Eligible peer
        assert!(snapshot.is_eligible(&make_overlay(6), &peer_manager));

        // In-progress
        assert!(!snapshot.is_eligible(&make_overlay(1), &peer_manager));

        // Queued
        assert!(!snapshot.is_eligible(&make_overlay(5), &peer_manager));

        // Banned peer (via PeerManager)
        peer_manager.store_discovered_peer(make_swarm_peer_minimal(2));
        peer_manager.ban(
            &make_overlay(2),
            vertex_swarm_api::BanCause::Requested,
            Some("test".into()),
        );
        assert!(!snapshot.is_eligible(&make_overlay(2), &peer_manager));
    }

    #[test]
    fn test_connected_peer_rejected_by_selector() {
        let limits = DepthAwareLimits::new(160, 3);
        let snapshot = CandidateSnapshot {
            limits: LimitsSnapshot::capture(&limits, d(0)),
            in_progress: HashSet::new(),
            queued: HashSet::new(),
        };

        let connected = test_proximity_index();
        let peer = make_overlay(4);
        let _ = connected.add(peer);

        let mut selector = CandidateSelector::new(&snapshot, &connected, 10);

        // Connected peer should be rejected
        assert!(!selector.try_add(peer));
        assert_eq!(selector.len(), 0);

        // Non-connected peer should be accepted
        assert!(selector.try_add(make_overlay(6)));
        assert_eq!(selector.len(), 1);
    }

    #[test]
    fn test_selector_prevents_duplicates() {
        let limits = DepthAwareLimits::new(160, 3);
        let snapshot = CandidateSnapshot {
            limits: LimitsSnapshot::capture(&limits, d(0)),
            in_progress: HashSet::new(),
            queued: HashSet::new(),
        };

        let connected = test_proximity_index();
        let mut selector = CandidateSelector::new(&snapshot, &connected, 10);

        let peer = make_overlay(1);
        assert!(selector.try_add(peer));
        assert!(!selector.try_add(peer)); // Duplicate

        assert_eq!(selector.len(), 1);
    }

    #[test]
    fn test_selector_respects_max() {
        let limits = DepthAwareLimits::new(160, 3);
        let snapshot = CandidateSnapshot {
            limits: LimitsSnapshot::capture(&limits, d(0)),
            in_progress: HashSet::new(),
            queued: HashSet::new(),
        };

        let connected = test_proximity_index();
        let mut selector = CandidateSelector::new(&snapshot, &connected, 2);

        assert!(selector.try_add(make_overlay(1)));
        assert!(selector.try_add(make_overlay(2)));
        assert!(!selector.try_add(make_overlay(3))); // At capacity

        assert!(selector.is_full());
        assert_eq!(selector.remaining(), 0);
    }

    #[test]
    fn test_bin_selection_tracking() {
        let limits = DepthAwareLimits::new(160, 3);
        let snapshot = CandidateSnapshot {
            limits: LimitsSnapshot::capture(&limits, d(0)),
            in_progress: HashSet::new(),
            queued: HashSet::new(),
        };

        let connected = test_proximity_index();
        let mut selector = CandidateSelector::new(&snapshot, &connected, 10);

        assert_eq!(selector.bin_selected(b(0)), 0);
        assert_eq!(selector.bin_selected(b(5)), 0);

        // After adding peers, bin counts should update
        // Note: try_add doesn't track bin (use try_add_with_bin_capacity for that)
        selector.try_add(make_overlay(1));
        assert_eq!(selector.bin_selected(b(0)), 0); // try_add doesn't track bins

        // We'd need a PeerManager to test try_add_with_bin_capacity
    }
}
