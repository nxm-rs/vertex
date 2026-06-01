//! Kademlia-based [`HandshakeAdmissionControl`] implementation.
//!
//! Delegates to [`KademliaRouting::admission_within_capacity`], which
//! evaluates the existing depth-aware saturation math against the in-flight
//! peer's bin. Plugs into the handshake behaviour via
//! [`HandshakeBehaviour::with_admission_control`].
//!
//! Unit 8 will extend the decision surface to return
//! [`AdmissionDecision::AcceptEvict`] for oversaturated neighborhoods; this
//! file only wires the saturation check.

use std::sync::Arc;

use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_net_handshake::{
    AdmissionDecision, AdmissionRejection, ConnectionDirection, HandshakeAdmissionControl,
    SharedAdmissionControl,
};
use vertex_swarm_peer::{SwarmAddress, SwarmNodeType};

use super::KademliaRouting;

/// Admission gate backed by the kademlia routing table.
///
/// Holds an `Arc` to the routing so the handshake behaviour can share the
/// same source of truth for capacity decisions as the dial planner.
pub(crate) struct KademliaAdmissionControl<I: SwarmIdentity> {
    routing: Arc<KademliaRouting<I>>,
}

impl<I: SwarmIdentity> KademliaAdmissionControl<I> {
    pub(crate) fn new(routing: Arc<KademliaRouting<I>>) -> Self {
        Self { routing }
    }
}

impl<I: SwarmIdentity> HandshakeAdmissionControl for KademliaAdmissionControl<I> {
    fn evaluate(
        &self,
        peer_overlay: &SwarmAddress,
        _node_type: SwarmNodeType,
        _direction: ConnectionDirection,
    ) -> AdmissionDecision {
        // Use the existing depth-aware saturation math. The in-flight peer
        // is already counted in routing's `effective_count` (outbound via
        // `try_reserve_dial`, inbound via `reserve_inbound` after handshake
        // completes), so we ask `admission_within_capacity` rather than
        // `should_accept_inbound` — the latter rejects any peer present in
        // the phases map.
        //
        // Direction is intentionally ignored for now: bee's Picker also
        // applies the saturation check symmetrically. Unit 8 will extend
        // this with neighborhood eviction (AcceptEvict).
        if self.routing.admission_within_capacity(peer_overlay) {
            AdmissionDecision::Accept
        } else {
            AdmissionDecision::Reject(AdmissionRejection::Saturated)
        }
    }
}

/// Convenience constructor returning the type-erased handle expected by
/// [`HandshakeBehaviour::with_admission_control`].
pub(crate) fn kademlia_admission_control<I: SwarmIdentity>(
    routing: Arc<KademliaRouting<I>>,
) -> SharedAdmissionControl {
    Arc::new(KademliaAdmissionControl::new(routing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kademlia::{KademliaConfig, RoutingCapacity};
    use vertex_swarm_peer_manager::PeerManager;
    use vertex_swarm_test_utils::MockIdentity;

    fn make_routing(
        base: SwarmAddress,
        config: KademliaConfig,
    ) -> Arc<KademliaRouting<MockIdentity>> {
        let identity = MockIdentity::with_overlay(base);
        let peer_manager = PeerManager::new(&identity);
        KademliaRouting::new(identity, config, peer_manager)
    }

    #[test]
    fn accepts_when_routing_has_capacity() {
        let base = SwarmAddress::with_first_byte(0x00);
        let routing = make_routing(base, KademliaConfig::default());
        let ac = KademliaAdmissionControl::new(routing);

        let decision = ac.evaluate(
            &SwarmAddress::with_first_byte(0x80),
            SwarmNodeType::Storer,
            ConnectionDirection::Inbound,
        );
        assert!(matches!(decision, AdmissionDecision::Accept));
    }

    #[test]
    fn accepts_when_bin_at_ceiling() {
        // At the ceiling exactly (counting our in-flight slot) is still
        // accepted; we only reject when strictly above.
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default()
            .with_nominal(1)
            .with_inbound_headroom(0);
        let routing = make_routing(base, config);

        // Reserve one slot in bin 0 (po=0) representing the in-flight peer.
        let peer = SwarmAddress::with_first_byte(0x80);
        RoutingCapacity::reserve_inbound(&*routing, &peer);

        let ac = KademliaAdmissionControl::new(routing);
        let decision = ac.evaluate(&peer, SwarmNodeType::Storer, ConnectionDirection::Inbound);
        assert!(matches!(decision, AdmissionDecision::Accept));
    }

    #[test]
    fn rejects_when_bin_oversaturated() {
        // Pile two in-flight peers into the same bin while the ceiling is 1
        // (nominal=1, headroom=0). The bin is strictly over capacity →
        // Reject.
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default()
            .with_nominal(1)
            .with_inbound_headroom(0);
        let routing = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80);
        let peer2 = SwarmAddress::with_first_byte(0xc0);
        RoutingCapacity::reserve_inbound(&*routing, &peer1);
        RoutingCapacity::reserve_inbound(&*routing, &peer2);

        let ac = KademliaAdmissionControl::new(routing);
        // Either in-flight peer should be rejected: both are in bin 0,
        // effective_count(0) = 2 > ceiling(0) = 1.
        let decision = ac.evaluate(&peer1, SwarmNodeType::Storer, ConnectionDirection::Inbound);
        assert!(matches!(
            decision,
            AdmissionDecision::Reject(AdmissionRejection::Saturated)
        ));
    }

    #[test]
    fn neighborhood_bin_always_accepts() {
        // Bins >= depth use ceiling = usize::MAX. Even with many entries the
        // saturation check must Accept; eviction is Unit 8's job.
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_nominal(1);
        let routing = make_routing(base, config);

        // Force depth to 0 so every bin is in neighborhood. (Default depth
        // is 0 anyway.) Add several peers in bin 7.
        for i in 0..3 {
            let mut bytes = [0u8; 32];
            bytes[0] = 0x01 + i; // po >= 6
            RoutingCapacity::reserve_inbound(&*routing, &SwarmAddress::from(bytes));
        }
        let ac = KademliaAdmissionControl::new(routing);
        let mut bytes = [0u8; 32];
        bytes[0] = 0x01;
        let decision = ac.evaluate(
            &SwarmAddress::from(bytes),
            SwarmNodeType::Storer,
            ConnectionDirection::Inbound,
        );
        assert!(matches!(decision, AdmissionDecision::Accept));
    }

    #[test]
    fn shared_handle_dispatches() {
        let base = SwarmAddress::with_first_byte(0x00);
        let routing = make_routing(base, KademliaConfig::default());
        let handle = kademlia_admission_control(routing);
        let decision = handle.evaluate(
            &SwarmAddress::with_first_byte(0x42),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
        );
        assert!(matches!(decision, AdmissionDecision::Accept));
    }
}
