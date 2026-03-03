//! NetworkBehaviour for handshake protocol.

use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
};

use libp2p::{
    Multiaddr, PeerId,
    swarm::{
        ConnectionClosed, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler, THandler,
        THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
};
use tracing::debug;
use vertex_swarm_api::SwarmIdentity;

use vertex_net_peer_registry::ConnectionDirection;

use crate::{
    AddressProvider, HandshakeError, HandshakeInfo,
    handler::{HandshakeConfig, HandshakeHandler, HandshakeCommand, HandshakeHandlerEvent},
};

/// Events emitted by HandshakeBehaviour.
#[derive(Debug)]
pub enum HandshakeEvent {
    /// Handshake completed successfully.
    Completed {
        peer_id: PeerId,
        connection_id: ConnectionId,
        direction: ConnectionDirection,
        info: Box<HandshakeInfo>,
    },
    /// Handshake failed.
    Failed {
        peer_id: PeerId,
        connection_id: ConnectionId,
        direction: ConnectionDirection,
        error: HandshakeError,
    },
}

/// Behaviour for the Swarm handshake protocol.
pub struct HandshakeBehaviour<I, A> {
    config: Arc<HandshakeConfig>,
    identity: Arc<I>,
    address_provider: Arc<A>,
    events: VecDeque<ToSwarm<HandshakeEvent, HandshakeCommand>>,
    /// Track direction per connection for event attribution.
    connection_directions: std::collections::HashMap<ConnectionId, ConnectionDirection>,
}

impl<I, A> HandshakeBehaviour<I, A>
where
    I: SwarmIdentity + 'static,
    A: AddressProvider + 'static,
{
    /// Create a new handshake behaviour with the given purpose label for metrics.
    pub fn new(identity: Arc<I>, address_provider: Arc<A>, purpose: &'static str) -> Self {
        Self {
            config: Arc::new(HandshakeConfig::new(purpose)),
            identity,
            address_provider,
            events: VecDeque::new(),
            connection_directions: std::collections::HashMap::new(),
        }
    }

    /// Create with custom config.
    pub fn with_config(mut self, config: HandshakeConfig) -> Self {
        self.config = Arc::new(config);
        self
    }

    /// Initiate handshake on a connection with a resolved address.
    pub fn initiate(&mut self, peer_id: PeerId, connection_id: ConnectionId, addr: Multiaddr) {
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(connection_id),
            event: HandshakeCommand::Initiate(addr),
        });
    }
}

impl<I, A> NetworkBehaviour for HandshakeBehaviour<I, A>
where
    I: SwarmIdentity + 'static,
    A: AddressProvider + 'static,
{
    type ConnectionHandler = HandshakeHandler<I, A>;
    type ToSwarm = HandshakeEvent;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, libp2p::swarm::ConnectionDenied> {
        debug!(%peer, ?connection_id, %remote_addr, "Creating inbound handshake handler");
        self.connection_directions.insert(connection_id, ConnectionDirection::Inbound);
        Ok(HandshakeHandler::new_inbound(
            self.config.clone(),
            self.identity.clone(),
            peer,
            remote_addr.clone(),
            self.address_provider.clone(),
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        _role_override: libp2p::core::Endpoint,
        _port_use: libp2p::core::transport::PortUse,
    ) -> Result<THandler<Self>, libp2p::swarm::ConnectionDenied> {
        debug!(%peer, ?connection_id, %addr, "Creating outbound handshake handler");
        self.connection_directions.insert(connection_id, ConnectionDirection::Outbound);
        Ok(HandshakeHandler::new_outbound(
            self.config.clone(),
            self.identity.clone(),
            peer,
            addr.clone(),
            self.address_provider.clone(),
        ))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionClosed(ConnectionClosed { connection_id, peer_id, .. }) => {
                self.connection_directions.remove(&connection_id);
                debug!(%peer_id, "Connection closed");
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        let direction = self.connection_directions
            .get(&connection_id)
            .copied()
            .unwrap_or(ConnectionDirection::Inbound);

        match event {
            HandshakeHandlerEvent::Completed { info } => {
                debug!(%peer_id, ?connection_id, ?direction, "Handshake completed");
                self.events.push_back(ToSwarm::GenerateEvent(HandshakeEvent::Completed {
                    peer_id,
                    connection_id,
                    direction,
                    info,
                }));
            }
            HandshakeHandlerEvent::Failed { error } => {
                debug!(%peer_id, ?connection_id, ?direction, ?error, "Handshake failed");
                self.events.push_back(ToSwarm::GenerateEvent(HandshakeEvent::Failed {
                    peer_id,
                    connection_id,
                    direction,
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
