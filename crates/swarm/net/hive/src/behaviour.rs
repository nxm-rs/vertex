//! NetworkBehaviour for hive protocol.

use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
};

use libp2p::{
    Multiaddr, PeerId,
    swarm::{
        ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler, THandler,
        THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
};
use strum::IntoStaticStr;
use tracing::debug;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_net_headers::ProtocolStreamError;
use vertex_swarm_peer::SwarmPeer;
use crate::handler::{HiveCommand, HiveHandler, HiveHandlerEvent};
use crate::protocol::{PeerCache, new_peer_cache};

/// Events emitted by HiveBehaviour.
#[derive(Debug, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum HiveEvent {
    /// Received peers from a connection.
    PeersReceived {
        peer_id: PeerId,
        connection_id: ConnectionId,
        peers: Vec<SwarmPeer>,
    },
    /// Error occurred.
    Error {
        peer_id: PeerId,
        connection_id: ConnectionId,
        error: ProtocolStreamError,
    },
}

/// Behaviour for the Swarm hive protocol.
pub struct HiveBehaviour<I> {
    identity: Arc<I>,
    cache: PeerCache,
    events: VecDeque<ToSwarm<HiveEvent, HiveCommand>>,
}

impl<I> HiveBehaviour<I>
where
    I: SwarmIdentity + 'static,
{
    /// Create a new hive behaviour.
    pub fn new(identity: Arc<I>) -> Self {
        Self {
            identity,
            cache: new_peer_cache(),
            events: VecDeque::new(),
        }
    }

    /// Broadcast peers to a specific connection.
    pub fn broadcast(&mut self, peer_id: PeerId, connection_id: ConnectionId, peers: Vec<SwarmPeer>) {
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(connection_id),
            event: HiveCommand::BroadcastPeers(peers),
        });
    }
}

impl<I> NetworkBehaviour for HiveBehaviour<I>
where
    I: SwarmIdentity + 'static,
{
    type ConnectionHandler = HiveHandler<I>;
    type ToSwarm = HiveEvent;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, libp2p::swarm::ConnectionDenied> {
        Ok(HiveHandler::new(
            self.identity.clone(),
            peer,
            self.cache.clone(),
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _addr: &Multiaddr,
        _role_override: libp2p::core::Endpoint,
        _port_use: libp2p::core::transport::PortUse,
    ) -> Result<THandler<Self>, libp2p::swarm::ConnectionDenied> {
        Ok(HiveHandler::new(
            self.identity.clone(),
            peer,
            self.cache.clone(),
        ))
    }

    fn on_swarm_event(&mut self, _event: FromSwarm) {}

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {
            HiveHandlerEvent::PeersReceived(peers) => {
                debug!(%peer_id, peer_count = peers.len(), "Hive: received peers");
                self.events.push_back(ToSwarm::GenerateEvent(HiveEvent::PeersReceived {
                    peer_id,
                    connection_id,
                    peers,
                }));
            }
            HiveHandlerEvent::Error(error) => {
                debug!(%peer_id, %error, "Hive: error");
                self.events.push_back(ToSwarm::GenerateEvent(HiveEvent::Error {
                    peer_id,
                    connection_id,
                    error,
                }));
            }
        }
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }
        Poll::Pending
    }
}
