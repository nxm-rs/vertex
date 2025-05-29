use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
};

use libp2p::{
    core::{transport::PortUse, Endpoint},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler,
        THandlerInEvent, ToSwarm,
    },
    Multiaddr, PeerId,
};
use vertex_node_core::args::NodeCommand;

use crate::{HandshakeCommand, HandshakeEvent, HandshakeHandler};

pub struct HandshakeBehaviour<const N: u64> {
    config: Arc<NodeCommand>,
    events: VecDeque<ToSwarm<HandshakeEvent<N>, HandshakeCommand>>,
}

impl<const N: u64> HandshakeBehaviour<N> {
    pub fn new(config: Arc<NodeCommand>) -> Self {
        Self {
            config,
            events: VecDeque::new(),
        }
    }
}

impl<const N: u64> NetworkBehaviour for HandshakeBehaviour<N> {
    type ConnectionHandler = HandshakeHandler<N>;
    type ToSwarm = HandshakeEvent<N>;

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(connection) => {
                // Start handshake for outbound connections.
                if connection.endpoint.is_dialer() {
                    self.events.push_back(ToSwarm::NotifyHandler {
                        peer_id: connection.peer_id,
                        handler: NotifyHandler::One(connection.connection_id),
                        event: HandshakeCommand::StartHandshake,
                    });
                }
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        event: HandshakeEvent<N>,
    ) {
        match event {
            HandshakeEvent::Completed(info) => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(HandshakeEvent::Completed(info)));
            }
            HandshakeEvent::Failed(error) => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(HandshakeEvent::Failed(error)));
            }
        }
    }

    fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }
        Poll::Pending
    }

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<libp2p::swarm::THandler<Self>, ConnectionDenied> {
        Ok(HandshakeHandler::new(
            self.config.clone(),
            peer,
            remote_addr,
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<libp2p::swarm::THandler<Self>, ConnectionDenied> {
        Ok(HandshakeHandler::new(self.config.clone(), peer, addr))
    }
}
