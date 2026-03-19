//! Gossip peer filtering and selection functions.

use vertex_net_local::IpCapability;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer::{AddressScope, SwarmPeer};
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_peer_score::SwarmScoringEvent;
use vertex_swarm_primitives::OverlayAddress;

use super::events::VerificationResult;
use crate::kademlia::peer_selection;

/// Reachability profile of a gossip recipient.
pub(crate) struct RecipientProfile {
    pub(crate) capability: IpCapability,
    pub(crate) scope: AddressScope,
}

impl RecipientProfile {
    /// Look up a peer's reachability profile from the peer manager.
    pub(crate) fn lookup<I: SwarmIdentity>(
        peer_manager: &PeerManager<I>,
        overlay: &OverlayAddress,
    ) -> Self {
        let capability = peer_manager
            .get_peer_capability(overlay)
            .unwrap_or(IpCapability::Dual);
        let scope = peer_manager
            .get_swarm_peer(overlay)
            .and_then(|p| p.max_scope())
            .unwrap_or(AddressScope::Public);
        Self { capability, scope }
    }
}

/// Map a verification result to its corresponding scoring event.
pub(crate) fn scoring_event_for(result: &VerificationResult) -> SwarmScoringEvent {
    match result {
        VerificationResult::Verified { .. } | VerificationResult::IdentityUpdated { .. } => {
            SwarmScoringEvent::GossipVerified
        }
        VerificationResult::DifferentPeerAtAddress { .. }
        | VerificationResult::Failed { .. } => SwarmScoringEvent::GossipInvalid,
        VerificationResult::Unreachable { .. } => SwarmScoringEvent::GossipUnreachable,
    }
}

/// Check if a peer's address scopes are compatible with gossiping to a recipient.
///
/// Public recipients only receive peers that have exclusively public addresses.
pub(crate) fn scope_eligible_for_recipient(peer: &SwarmPeer, recipient_scope: AddressScope) -> bool {
    if recipient_scope != AddressScope::Public {
        return true;
    }
    let has_non_public = peer.has_scope(AddressScope::Private)
        || peer.has_scope(AddressScope::LinkLocal)
        || peer.has_scope(AddressScope::Loopback);
    !has_non_public && peer.has_scope(AddressScope::Public)
}

/// Look up a peer's IP capability, defaulting to dual-stack if unknown.
pub(crate) fn peer_capability<I: SwarmIdentity>(
    peer_manager: &PeerManager<I>,
    overlay: &OverlayAddress,
) -> IpCapability {
    peer_manager
        .get_peer_capability(overlay)
        .unwrap_or(IpCapability::Dual)
}

/// Filter peers by recipient's IP capability and address scope.
///
/// Returns references to eligible peers — caller decides whether to clone.
pub(crate) fn filter_peers_for_recipient<'a, I: SwarmIdentity>(
    peers: &'a [SwarmPeer],
    recipient: &RecipientProfile,
    peer_manager: &PeerManager<I>,
) -> Vec<&'a SwarmPeer> {
    peers
        .iter()
        .filter(|peer| {
            let peer_overlay = OverlayAddress::from(*peer.overlay());
            let cap = peer_capability(peer_manager, &peer_overlay);
            recipient.capability.can_reach(&cap)
                && scope_eligible_for_recipient(peer, recipient.scope)
        })
        .collect()
}

/// Select and filter peers for a distant (non-neighbor) recipient.
pub(crate) fn select_peers_for_distant<I: SwarmIdentity>(
    local_overlay: &OverlayAddress,
    peer_manager: &PeerManager<I>,
    recipient_overlay: OverlayAddress,
    recipient: &RecipientProfile,
) -> Vec<SwarmPeer> {
    let candidates =
        peer_selection::select_for_distant(local_overlay, peer_manager, recipient_overlay);

    candidates
        .into_iter()
        .filter(|(peer, _bin)| {
            let peer_overlay = OverlayAddress::from(*peer.overlay());
            let cap = peer_capability(peer_manager, &peer_overlay);
            recipient.capability.can_reach(&cap)
                && scope_eligible_for_recipient(peer, recipient.scope)
        })
        .map(|(peer, _bin)| peer)
        .collect()
}

/// Detect a depth decrease. Returns `Some((old_depth, new_depth))` when depth decreased.
pub(crate) fn detect_depth_decrease(current_depth: u8, last_depth: &mut u8) -> Option<(u8, u8)> {
    let current = current_depth;
    let old = *last_depth;
    if current == old {
        return None;
    }
    *last_depth = current;
    if current >= old {
        return None;
    }
    Some((old, current))
}
