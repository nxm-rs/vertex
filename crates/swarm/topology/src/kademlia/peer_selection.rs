//! Kademlia-aware peer selection for gossip exchange.

use std::collections::HashSet;

use vertex_swarm_api::{SwarmIdentity, SwarmNodeType};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_primitives::OverlayAddress;

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
    depth: u8,
) -> Vec<OverlayAddress> {
    connection_registry
        .active_ids()
        .into_iter()
        .filter(|overlay| {
            local_overlay.proximity(overlay) >= depth
                && peer_manager.node_type(overlay) == Some(SwarmNodeType::Storer)
        })
        .collect()
}

/// Known storers in neighborhood, optionally excluding one overlay.
pub(crate) fn known_neighborhood_peers<I: SwarmIdentity>(
    _local_overlay: &OverlayAddress,
    peer_manager: &PeerManager<I>,
    depth: u8,
    exclude: Option<&OverlayAddress>,
) -> Vec<SwarmPeer> {
    let max_po = peer_manager.index().max_po();
    let mut overlays = Vec::new();
    for po in depth..=max_po {
        for overlay in peer_manager.storer_overlays_in_bin(po, usize::MAX) {
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
/// Returns unfiltered peers with their bin — caller applies IP/scope filtering.
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
            let peer = peer_manager.get_swarm_peer(overlay)?;
            let proximity_to_recipient = recipient.proximity(overlay);
            let bin = local_overlay.proximity(overlay);
            Some((peer, proximity_to_recipient, bin))
        })
        .collect();

    if storers.is_empty() {
        return selected;
    }

    // Phase 1: Top CLOSE_PEERS_COUNT by proximity to recipient (O(p) partition)
    let mut indices: Vec<usize> = (0..storers.len()).collect();
    if indices.len() > CLOSE_PEERS_COUNT {
        indices.select_nth_unstable_by(CLOSE_PEERS_COUNT, |&a, &b| storers[b].1.cmp(&storers[a].1));
        indices[..CLOSE_PEERS_COUNT].sort_by(|&a, &b| storers[b].1.cmp(&storers[a].1));
    } else {
        indices.sort_by(|&a, &b| storers[b].1.cmp(&storers[a].1));
    }

    for &idx in indices.iter().take(CLOSE_PEERS_COUNT) {
        if selected_indices.insert(idx) {
            selected.push((storers[idx].0.clone(), storers[idx].2));
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

    #[test]
    fn test_connected_neighbors_empty_when_no_connections() {
        let ctx = TopologyTestContext::new().with_peers();
        let neighbors = connected_neighbors(
            &ctx.local_overlay,
            &ctx.peer_manager,
            &ctx.connection_registry,
            0,
        );
        assert!(neighbors.is_empty());
    }

    #[test]
    fn test_select_for_distant_no_duplicates() {
        let ctx = TopologyTestContext::new().with_peers();
        let recipient = test_overlay(0xFF);

        let selected = select_for_distant(
            &ctx.local_overlay,
            &ctx.peer_manager,
            recipient,
        );

        let unique: HashSet<_> = selected.iter().map(|(p, _)| *p.overlay()).collect();
        assert_eq!(unique.len(), selected.len());
    }
}
