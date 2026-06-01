//! Composed protocol behaviours for topology.
//!
//! Uses libp2p's derive(NetworkBehaviour) to compose handshake, hive, and pingpong
//! into a single behaviour with automatic handler composition.

use std::sync::Arc;

use libp2p::swarm::NetworkBehaviour;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_net_handshake::{HandshakeBehaviour, HandshakeEvent};
use vertex_swarm_net_hive::{HiveBehaviour, HiveEvent};
use vertex_swarm_net_pingpong::{PingpongBehaviour, PingpongEvent};

use crate::nat_discovery::LocalAddressManager;

/// Combined event from all protocol behaviours.
#[derive(Debug)]
pub enum ProtocolEvent {
    Handshake(HandshakeEvent),
    Hive(HiveEvent),
    Pingpong(PingpongEvent),
}

impl ProtocolEvent {
    /// Extract peer_id and connection_id from any protocol event.
    pub(crate) fn peer_connection(&self) -> (libp2p::PeerId, libp2p::swarm::ConnectionId) {
        match self {
            Self::Handshake(HandshakeEvent::Completed {
                peer_id,
                connection_id,
                ..
            }) => (*peer_id, *connection_id),
            Self::Handshake(HandshakeEvent::Failed {
                peer_id,
                connection_id,
                ..
            }) => (*peer_id, *connection_id),
            Self::Hive(HiveEvent::PeersReceived {
                peer_id,
                connection_id,
                ..
            }) => (*peer_id, *connection_id),
            Self::Hive(HiveEvent::Error {
                peer_id,
                connection_id,
                ..
            }) => (*peer_id, *connection_id),
            Self::Pingpong(PingpongEvent::Pong {
                peer_id,
                connection_id,
                ..
            }) => (*peer_id, *connection_id),
            Self::Pingpong(PingpongEvent::RttObserved { peer_id, .. }) => {
                // RttObserved carries no connection id. The downstream
                // `on_pingpong_rtt` handler routes purely off the peer id;
                // the connection id is a placeholder that never reaches a
                // per-connection sink.
                (*peer_id, libp2p::swarm::ConnectionId::new_unchecked(0))
            }
            Self::Pingpong(PingpongEvent::PingReceived {
                peer_id,
                connection_id,
            }) => (*peer_id, *connection_id),
            Self::Pingpong(PingpongEvent::Error {
                peer_id,
                connection_id,
                ..
            }) => (*peer_id, *connection_id),
            // `PingpongEvent` is `#[non_exhaustive]`. Future variants we do
            // not yet recognise are routed with sentinel ids; the dispatcher
            // logs and discards them via the wildcard arm in
            // `protocol_handlers::process_protocol_event`.
            Self::Pingpong(_) => unreachable_pingpong_event(),
        }
    }
}

/// Sentinel fallback for forward-compatibility with new `PingpongEvent`
/// variants. Never produces a useful routing target — the dispatcher's
/// wildcard arm discards events that hit this path.
fn unreachable_pingpong_event() -> (libp2p::PeerId, libp2p::swarm::ConnectionId) {
    (
        libp2p::PeerId::random(),
        libp2p::swarm::ConnectionId::new_unchecked(0),
    )
}

impl From<HandshakeEvent> for ProtocolEvent {
    fn from(event: HandshakeEvent) -> Self {
        ProtocolEvent::Handshake(event)
    }
}

impl From<HiveEvent> for ProtocolEvent {
    fn from(event: HiveEvent) -> Self {
        ProtocolEvent::Hive(event)
    }
}

impl From<PingpongEvent> for ProtocolEvent {
    fn from(event: PingpongEvent) -> Self {
        ProtocolEvent::Pingpong(event)
    }
}

/// Composed protocol behaviours.
///
/// This struct uses libp2p's derive macro to automatically compose
/// the three protocol handlers into a single connection handler.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "ProtocolEvent")]
pub struct ProtocolBehaviours<I>
where
    I: SwarmIdentity + Clone + 'static,
{
    pub(crate) handshake: HandshakeBehaviour<I, LocalAddressManager>,
    pub(crate) hive: HiveBehaviour<I>,
    pub(crate) pingpong: PingpongBehaviour,
}

impl<I> ProtocolBehaviours<I>
where
    I: SwarmIdentity + Clone + 'static,
{
    /// Create new composed protocol behaviours.
    pub(crate) fn new(identity: Arc<I>, address_provider: Arc<LocalAddressManager>) -> Self {
        Self {
            handshake: HandshakeBehaviour::new(identity.clone(), address_provider, "topology"),
            hive: HiveBehaviour::new(identity),
            pingpong: PingpongBehaviour::new(),
        }
    }
}
