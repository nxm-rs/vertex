use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
};

use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler,
        THandlerInEvent, ToSwarm,
    },
};

use crate::{HandshakeCommand, HandshakeConfig, HandshakeEvent, HandshakeHandler};

pub struct HandshakeBehaviour<C: HandshakeConfig> {
    config: Arc<C>,
    events: VecDeque<ToSwarm<HandshakeEvent, HandshakeCommand>>,
}

impl<C: HandshakeConfig> HandshakeBehaviour<C> {
    pub fn new(config: Arc<C>) -> Self {
        Self {
            config,
            events: VecDeque::new(),
        }
    }
}

impl<C: HandshakeConfig> NetworkBehaviour for HandshakeBehaviour<C> {
    type ConnectionHandler = HandshakeHandler<C>;
    type ToSwarm = HandshakeEvent;

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(connection) => {
                // Start handshake for outbound connections.
                if connection.endpoint.is_dialer() {
                    // Use the resolved remote address (actual IP), not the dial address
                    // (which might be a DNS address like /dnsaddr/mainnet.ethswarm.org)
                    let resolved_addr = connection.endpoint.get_remote_address().clone();
                    self.events.push_back(ToSwarm::NotifyHandler {
                        peer_id: connection.peer_id,
                        handler: NotifyHandler::One(connection.connection_id),
                        event: HandshakeCommand::StartHandshake(resolved_addr),
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
        event: HandshakeEvent,
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
