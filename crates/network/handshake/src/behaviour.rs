use std::{
    collections::{HashMap, VecDeque},
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

use crate::{
    HandshakeCommand, HandshakeConfig, HandshakeEvent, HandshakeHandler, HandshakeInfo, PeerState,
};

pub struct HandshakeBehaviour<const N: u64> {
    config: Arc<HandshakeConfig<N>>,
    handshaked_peers: HashMap<PeerId, PeerState<N>>,
    events: VecDeque<ToSwarm<HandshakeEvent<N>, HandshakeCommand>>,
}

impl<const N: u64> HandshakeBehaviour<N> {
    pub fn new(config: Arc<HandshakeConfig<N>>) -> Self {
        Self {
            config,
            handshaked_peers: HashMap::new(),
            events: VecDeque::new(),
        }
    }

    pub fn peer_info(&self, peer: &PeerId) -> Option<&HandshakeInfo<N>> {
        self.handshaked_peers.get(peer).map(|state| &state.info)
    }

    pub fn handshaked_peers(&self) -> impl Iterator<Item = (&PeerId, &HandshakeInfo<N>)> {
        self.handshaked_peers
            .iter()
            .map(|(id, state)| (id, &state.info))
    }

    pub fn is_peer_handshaked(&self, peer: &PeerId) -> bool {
        self.handshaked_peers.contains_key(peer)
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
        peer_id: PeerId,
        connection: ConnectionId,
        event: HandshakeEvent<N>,
    ) {
        match event {
            HandshakeEvent::Completed(info) => {
                // Store peer info on successful handshake
                self.handshaked_peers.insert(
                    peer_id,
                    PeerState {
                        info: info.clone(),
                        connections: vec![connection],
                    },
                );
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
        _connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<libp2p::swarm::THandler<Self>, ConnectionDenied> {
        Ok(HandshakeHandler::new(
            self.config.clone(),
            peer,
            remote_addr.clone(),
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
        port_use: PortUse,
    ) -> Result<libp2p::swarm::THandler<Self>, ConnectionDenied> {
        Ok(HandshakeHandler::new(
            self.config.clone(),
            peer,
            addr.clone(),
        ))
    }
}
