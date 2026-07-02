//! Kademlia-aware peer selection for gossip exchange.

use std::collections::{BTreeMap, HashSet};

use vertex_swarm_api::{SwarmIdentity, SwarmNodeType};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_primitives::{Bin, NeighborhoodDepth, OverlayAddress, neighborhood_bins};

use crate::behaviour::ConnectionRegistry;

/// Maximum peers to send to distant (non-neighbor) peers.
const MAX_PEERS_FOR_DISTANT: usize = 16;

/// Number of peers close to recipient's overlay to include.
const CLOSE_PEERS_COUNT: usize = 4;

/// Active storer peers with proximity >= depth.
pub(crate) fn connected_neighbors<I: SwarmIdentity>(
    local_overlay: &OverlayAddress,
    peer_manager: &PeerManager<I>,
    connection_registry: &ConnectionRegistry,
    depth: NeighborhoodDepth,
) -> Vec<OverlayAddress> {
    connection_registry
        .active_ids()
        .into_iter()
        .filter(|overlay| {
            depth.contains(Bin::from(local_overlay.proximity(overlay)))
                && peer_manager.node_type(overlay) == Some(SwarmNodeType::Storer)
        })
        .collect()
}

/// Connected clients: gossip recipients only, never gossiped about.
pub(crate) fn connected_clients<I: SwarmIdentity>(
    peer_manager: &PeerManager<I>,
    connection_registry: &ConnectionRegistry,
) -> Vec<OverlayAddress> {
    connection_registry
        .active_ids()
        .into_iter()
        .filter(|overlay| peer_manager.node_type(overlay) == Some(SwarmNodeType::Client))
        .collect()
}

/// A bounded per-bin sample of connected storers, for announcing a newly
/// connected storer to the wider table without a per-connect broadcast storm.
///
/// Groups active storer connections by their bin relative to `local_overlay`
/// and takes up to `per_bin` from each, excluding `exclude` (the newcomer).
/// The sample is deterministic (no random subset, so the broadcast fan-out is
/// testable and needs no rng on wasm) but keyed on `subject`: within each bin
/// the storers closest to the announced peer by XOR distance are chosen, so the
/// recipient set rotates with who is announced rather than always favouring the
/// same low-overlay peers, and the announcement flows toward the newcomer's own
/// neighbourhood. The anti-amplification bound is the per-bin cap either way.
pub(crate) fn connected_storer_sample<I: SwarmIdentity>(
    local_overlay: &OverlayAddress,
    peer_manager: &PeerManager<I>,
    connection_registry: &ConnectionRegistry,
    per_bin: usize,
    exclude: &OverlayAddress,
    subject: &OverlayAddress,
) -> Vec<OverlayAddress> {
    if per_bin == 0 {
        return Vec::new();
    }
    let mut by_bin: BTreeMap<u8, Vec<OverlayAddress>> = BTreeMap::new();
    for overlay in connection_registry.active_ids() {
        if &overlay == exclude {
            continue;
        }
        if peer_manager.node_type(&overlay) != Some(SwarmNodeType::Storer) {
            continue;
        }
        let bin = local_overlay.proximity(&overlay).get();
        by_bin.entry(bin).or_default().push(overlay);
    }
    let mut sample = Vec::new();
    for (_bin, mut overlays) in by_bin {
        overlays.sort_unstable_by_key(|overlay| subject.distance(overlay));
        sample.extend(overlays.into_iter().take(per_bin));
    }
    sample
}

/// Known storers in neighborhood, optionally excluding one overlay.
pub(crate) fn known_neighborhood_peers<I: SwarmIdentity>(
    _local_overlay: &OverlayAddress,
    peer_manager: &PeerManager<I>,
    depth: NeighborhoodDepth,
    exclude: Option<&OverlayAddress>,
) -> Vec<SwarmPeer> {
    let max_bin = Bin::new(peer_manager.index().max_po()).unwrap_or(Bin::MAX);
    let mut overlays = Vec::new();
    for bin in neighborhood_bins(depth, max_bin) {
        for overlay in peer_manager.storer_overlays_in_bin(bin) {
            if exclude.is_some_and(|e| &overlay == e) {
                continue;
            }
            overlays.push(overlay);
        }
    }
    peer_manager.get_swarm_peers(&overlays)
}

/// 3-phase distant selection: close-to-recipient + per-bin + fill.
///
/// Returns unfiltered peers with their bin; caller applies IP/scope filtering.
pub(crate) fn select_for_distant<I: SwarmIdentity>(
    local_overlay: &OverlayAddress,
    peer_manager: &PeerManager<I>,
    recipient: OverlayAddress,
) -> Vec<(SwarmPeer, u8)> {
    let mut selected = Vec::with_capacity(MAX_PEERS_FOR_DISTANT);
    let mut selected_indices: HashSet<usize> = HashSet::with_capacity(MAX_PEERS_FOR_DISTANT);
    let mut added_bins: HashSet<u8> = HashSet::new();

    let all_storers = peer_manager.known_storer_overlays();

    let storers: Vec<_> = all_storers
        .iter()
        .filter_map(|overlay| {
            let peer = peer_manager.swarm_peer(overlay)?;
            let proximity_to_recipient = recipient.proximity(overlay).get();
            let bin = local_overlay.proximity(overlay).get();
            Some((peer, proximity_to_recipient, bin))
        })
        .collect();

    if storers.is_empty() {
        return selected;
    }

    // Phase 1: Top CLOSE_PEERS_COUNT by proximity to recipient (O(p) partition)
    let mut indices: Vec<usize> = (0..storers.len()).collect();
    let cmp_proximity = |a: &usize, b: &usize| {
        let pa = storers.get(*a).map(|s| s.1);
        let pb = storers.get(*b).map(|s| s.1);
        pb.cmp(&pa)
    };
    if indices.len() > CLOSE_PEERS_COUNT {
        indices.select_nth_unstable_by(CLOSE_PEERS_COUNT, |a, b| cmp_proximity(a, b));
        if let Some(top) = indices.get_mut(..CLOSE_PEERS_COUNT) {
            top.sort_by(|a, b| cmp_proximity(a, b));
        }
    } else {
        indices.sort_by(|a, b| cmp_proximity(a, b));
    }

    for &idx in indices.iter().take(CLOSE_PEERS_COUNT) {
        if selected_indices.insert(idx)
            && let Some(entry) = storers.get(idx)
        {
            selected.push((entry.0.clone(), entry.2));
        }
    }

    // Phase 2: One peer per bin (routing diversity)
    for (idx, (peer, _, bin)) in storers.iter().enumerate() {
        if selected.len() >= MAX_PEERS_FOR_DISTANT {
            break;
        }
        if !selected_indices.contains(&idx) && added_bins.insert(*bin) {
            selected_indices.insert(idx);
            selected.push((peer.clone(), *bin));
        }
    }

    // Phase 3: Fill remaining slots
    for (idx, (peer, _, bin)) in storers.iter().enumerate() {
        if selected.len() >= MAX_PEERS_FOR_DISTANT {
            break;
        }
        if selected_indices.insert(idx) {
            selected.push((peer.clone(), *bin));
        }
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TopologyTestContext;
    use vertex_swarm_test_utils::test_overlay;

    /// The per-bin sample orders candidates by XOR distance to the subject, so
    /// two different subjects pick a different closest-first order over the same
    /// candidate set: no candidate is globally excluded the way an absolute
    /// overlay sort would exclude the high-overlay peers of a full bin.
    #[test]
    fn the_sample_order_rotates_with_the_subject() {
        let candidates = [test_overlay(0x11), test_overlay(0x22), test_overlay(0x44)];

        let mut for_a = candidates;
        for_a.sort_unstable_by_key(|o| test_overlay(0x10).distance(o));

        let mut for_b = candidates;
        for_b.sort_unstable_by_key(|o| test_overlay(0x44).distance(o));

        assert_ne!(
            for_a, for_b,
            "the closest-first order must depend on the subject"
        );
        assert_eq!(
            for_b[0],
            test_overlay(0x44),
            "each subject leads with the candidate nearest to it"
        );
    }

    #[test]
    fn test_connected_neighbors_empty_when_no_connections() {
        let ctx = TopologyTestContext::new().with_peers();
        let neighbors = connected_neighbors(
            &ctx.local_overlay,
            &ctx.peer_manager,
            &ctx.connection_registry,
            NeighborhoodDepth::ZERO,
        );
        assert!(neighbors.is_empty());
    }

    #[test]
    fn test_select_for_distant_no_duplicates() {
        let ctx = TopologyTestContext::new().with_peers();
        let recipient = test_overlay(0xFF);

        let selected = select_for_distant(&ctx.local_overlay, &ctx.peer_manager, recipient);

        let unique: HashSet<_> = selected.iter().map(|(p, _)| *p.overlay()).collect();
        assert_eq!(unique.len(), selected.len());
    }
}
