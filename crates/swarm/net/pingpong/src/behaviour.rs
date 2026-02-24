//! NetworkBehaviour for pingpong protocol.

use std::{
    collections::VecDeque,
    task::{Context, Poll},
    time::Duration,
};

use libp2p::{
    Multiaddr, PeerId,
    swarm::{
        ConnectionClosed, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler, THandler,
        THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
};
use strum::IntoStaticStr;
use tracing::{debug, trace};

use vertex_swarm_net_headers::ProtocolStreamError;
use crate::handler::{PingpongCommand, PingpongConfig, PingpongHandler, PingpongHandlerEvent};

/// Events emitted by PingpongBehaviour.
#[derive(Debug, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum PingpongEvent {
    /// Pong received with RTT measurement.
    Pong {
        peer_id: PeerId,
        connection_id: ConnectionId,
        /// The pong response string.
        response: String,
        /// Round-trip time.
        rtt: Duration,
    },
    /// Responded to incoming ping.
    PingReceived {
        peer_id: PeerId,
        connection_id: ConnectionId,
    },
    /// Error occurred.
    Error {
        peer_id: PeerId,
        connection_id: ConnectionId,
        error: ProtocolStreamError,
    },
}

/// Behaviour for the Swarm pingpong protocol.
pub struct PingpongBehaviour {
    config: PingpongConfig,
    events: VecDeque<ToSwarm<PingpongEvent, PingpongCommand>>,
}

impl PingpongBehaviour {
    pub fn new() -> Self {
        Self {
            config: PingpongConfig::default(),
            events: VecDeque::new(),
        }
    }

    pub fn with_config(config: PingpongConfig) -> Self {
        Self {
            config,
            events: VecDeque::new(),
        }
    }

    /// Send a ping to a specific connection.
    pub fn ping(&mut self, peer_id: PeerId, connection_id: ConnectionId, greeting: Option<String>) {
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(connection_id),
            event: PingpongCommand::Ping { greeting },
        });
    }

    /// Send a ping to any connection with a peer.
    pub fn ping_peer(&mut self, peer_id: PeerId, greeting: Option<String>) {
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::Any,
            event: PingpongCommand::Ping { greeting },
        });
    }
}

impl Default for PingpongBehaviour {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkBehaviour for PingpongBehaviour {
    type ConnectionHandler = PingpongHandler;
    type ToSwarm = PingpongEvent;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, libp2p::swarm::ConnectionDenied> {
        Ok(PingpongHandler::new(self.config.clone(), peer))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _addr: &Multiaddr,
        _role_override: libp2p::core::Endpoint,
        _port_use: libp2p::core::transport::PortUse,
    ) -> Result<THandler<Self>, libp2p::swarm::ConnectionDenied> {
        Ok(PingpongHandler::new(self.config.clone(), peer))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        if let FromSwarm::ConnectionClosed(ConnectionClosed { peer_id, .. }) = event {
            trace!(%peer_id, "Pingpong: connection closed");
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {
            PingpongHandlerEvent::Pong { response, rtt } => {
                debug!(%peer_id, ?rtt, "Pingpong: pong received");
                self.events
                    .push_back(ToSwarm::GenerateEvent(PingpongEvent::Pong {
                        peer_id,
                        connection_id,
                        response,
                        rtt,
                    }));
            }
            PingpongHandlerEvent::PingReceived => {
                trace!(%peer_id, "Pingpong: responded to ping");
                self.events
                    .push_back(ToSwarm::GenerateEvent(PingpongEvent::PingReceived {
                        peer_id,
                        connection_id,
                    }));
            }
            PingpongHandlerEvent::Error(error) => {
                debug!(%peer_id, %error, "Pingpong: error");
                self.events
                    .push_back(ToSwarm::GenerateEvent(PingpongEvent::Error {
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
