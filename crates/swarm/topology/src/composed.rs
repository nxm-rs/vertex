//! Composed protocol behaviours for topology.
//!
//! Uses libp2p's derive(NetworkBehaviour) to compose handshake, hive, and pingpong
//! into a single behaviour with automatic handler composition.

use std::sync::Arc;

use libp2p::swarm::NetworkBehaviour;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_net_handshake::{HandshakeBehaviour, HandshakeEvent};
use vertex_swarm_net_hive::{
    DiscardSilently, HiveBehaviour, HiveEvent, HivePeerHandler, LearnAndDial,
};
use vertex_swarm_net_pingpong::{PingpongBehaviour, PingpongEvent};
use vertex_swarm_primitives::SwarmNodeType;

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
            Self::Pingpong(PingpongEvent::PingReceived {
                peer_id,
                connection_id,
            }) => (*peer_id, *connection_id),
            Self::Pingpong(PingpongEvent::Error {
                peer_id,
                connection_id,
                ..
            }) => (*peer_id, *connection_id),
        }
    }
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
    ///
    /// The hive [`HivePeerHandler`] is picked from the local node type:
    /// bootnodes drop inbound peer gossip without ingesting it, every other
    /// role learns and may dial. Outbound broadcasting runs in either case.
    pub(crate) fn new(identity: Arc<I>, address_provider: Arc<LocalAddressManager>) -> Self {
        let peer_handler: Arc<dyn HivePeerHandler> = match identity.node_type() {
            SwarmNodeType::Bootnode => Arc::new(DiscardSilently),
            SwarmNodeType::Client | SwarmNodeType::Storer => Arc::new(LearnAndDial),
        };

        Self {
            handshake: HandshakeBehaviour::new(identity.clone(), address_provider, "topology"),
            hive: HiveBehaviour::with_peer_handler(identity, peer_handler),
            pingpong: PingpongBehaviour::new(),
        }
    }
}
