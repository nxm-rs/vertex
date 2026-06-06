//! Kademlia-backed [`HandshakeAdmissionControl`].
//!
//! Delegates to [`KademliaRouting::admission_within_capacity`] with a
//! direction-aware `extra` count so the in-flight peer is modelled
//! correctly on both sides of the handshake. Plugs into the handshake
//! behaviour through
//! [`HandshakeBehaviour::with_admission_control`](vertex_swarm_net_handshake::HandshakeBehaviour::with_admission_control).

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
/// Holds an `Arc` to the routing so the handshake behaviour shares the
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
        direction: ConnectionDirection,
    ) -> AdmissionDecision {
        // Inbound is not yet reserved at gate time; outbound was
        // reserved at dial planning. See
        // `KademliaRouting::admission_within_capacity` for the full
        // accounting argument.
        let extra = match direction {
            ConnectionDirection::Inbound => 1,
            ConnectionDirection::Outbound => 0,
        };
        if self.routing.admission_within_capacity(peer_overlay, extra) {
            AdmissionDecision::Accept
        } else {
            AdmissionDecision::Reject(AdmissionRejection::Saturated)
        }
    }
}

/// Convenience constructor returning the type-erased handle expected by
/// [`HandshakeBehaviour::with_admission_control`](vertex_swarm_net_handshake::HandshakeBehaviour::with_admission_control).
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
    fn inbound_accepts_at_ceiling_minus_one() {
        // Capacity 1 (nominal=1, headroom=0). With zero peers reserved
        // the in-flight inbound peer fills the only slot exactly.
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default()
            .with_nominal(1)
            .with_inbound_headroom(0);
        let routing = make_routing(base, config);

        let ac = KademliaAdmissionControl::new(routing);
        let peer = SwarmAddress::with_first_byte(0x80);
        let decision = ac.evaluate(&peer, SwarmNodeType::Storer, ConnectionDirection::Inbound);
        assert!(matches!(decision, AdmissionDecision::Accept));
    }

    #[test]
    fn inbound_rejects_when_bin_already_full() {
        // Capacity 1 and one peer already reserved into po=0. The
        // in-flight inbound peer (also po=0) would push the bin over
        // ceiling.
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default()
            .with_nominal(1)
            .with_inbound_headroom(0);
        let routing = make_routing(base, config);

        let occupied = SwarmAddress::with_first_byte(0xc0);
        RoutingCapacity::reserve_inbound(&*routing, &occupied);

        let ac = KademliaAdmissionControl::new(routing);
        let peer = SwarmAddress::with_first_byte(0x80);
        let decision = ac.evaluate(&peer, SwarmNodeType::Storer, ConnectionDirection::Inbound);
        assert!(matches!(
            decision,
            AdmissionDecision::Reject(AdmissionRejection::Saturated)
        ));
    }

    #[test]
    fn outbound_accepts_at_ceiling_after_dial_reserve() {
        // Capacity 1. The outbound peer reserved its slot via
        // `try_reserve_dial`, so `effective_count` already includes it.
        // Admission must accept because the slot it occupies is its own.
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default()
            .with_nominal(1)
            .with_inbound_headroom(0);
        let routing = make_routing(base, config);

        let peer = SwarmAddress::with_first_byte(0x80);
        assert!(RoutingCapacity::try_reserve_dial(
            &*routing,
            &peer,
            SwarmNodeType::Storer,
        ));

        let ac = KademliaAdmissionControl::new(routing);
        let decision = ac.evaluate(&peer, SwarmNodeType::Storer, ConnectionDirection::Outbound);
        assert!(matches!(decision, AdmissionDecision::Accept));
    }

    #[test]
    fn outbound_rejects_when_bin_oversaturated() {
        // Two outbound dials reserved into the same bin while
        // ceiling=1. The second peer should have been rejected by
        // `try_reserve_dial` already, but if a stale caller passes it
        // to the gate we still want a clean reject.
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default()
            .with_nominal(1)
            .with_inbound_headroom(0);
        let routing = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80);
        let peer2 = SwarmAddress::with_first_byte(0xc0);
        assert!(RoutingCapacity::try_reserve_dial(
            &*routing,
            &peer1,
            SwarmNodeType::Storer,
        ));
        // Force-reserve peer2 even though try_reserve_dial would refuse,
        // to reach the oversaturated state.
        RoutingCapacity::reserve_inbound(&*routing, &peer2);

        let ac = KademliaAdmissionControl::new(routing);
        let decision = ac.evaluate(&peer1, SwarmNodeType::Storer, ConnectionDirection::Outbound);
        assert!(matches!(
            decision,
            AdmissionDecision::Reject(AdmissionRejection::Saturated)
        ));
    }

    #[test]
    fn neighborhood_bin_always_accepts() {
        // Bins inside the neighborhood (ceiling = usize::MAX) accept
        // unconditionally; oversaturation there is a separate concern.
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_nominal(1);
        let routing = make_routing(base, config);

        for i in 0..3 {
            let mut bytes = [0u8; 32];
            bytes[0] = 0x01 + i;
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
