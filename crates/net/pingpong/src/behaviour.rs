//! NetworkBehaviour implementation for the pingpong protocol.
//!
//! This behaviour handles the Swarm pingpong protocol for measuring RTT
//! and verifying connection liveness.

use std::{
    collections::{HashMap, VecDeque},
    task::{Context, Poll},
    time::Duration,
};

use libp2p::{
    core::{transport::PortUse, Endpoint},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler, THandlerInEvent,
        ToSwarm,
    },
    Multiaddr, PeerId,
};

use crate::handler::{Command, Config as HandlerConfig, Event as HandlerEvent, Handler};

/// Events emitted by the pingpong behaviour.
#[derive(Debug)]
pub enum PingpongEvent {
    /// Successfully received a pong response.
    Pong {
        /// The peer that responded.
        peer_id: PeerId,
        /// The round-trip time.
        rtt: Duration,
    },
    /// Responded to an incoming ping from a peer.
    PingReceived {
        /// The peer that sent the ping.
        peer_id: PeerId,
    },
    /// Pingpong exchange failed.
    Error {
        /// The peer involved in the failure.
        peer_id: PeerId,
        /// Error description.
        error: String,
    },
}

/// Configuration for the pingpong behaviour.
#[derive(Debug, Clone)]
pub struct PingpongConfig {
    /// Handler configuration.
    pub handler_config: HandlerConfig,
}

impl Default for PingpongConfig {
    fn default() -> Self {
        Self {
            handler_config: HandlerConfig::default(),
        }
    }
}

/// NetworkBehaviour for the Swarm pingpong protocol.
///
/// This behaviour provides RTT measurement via the Swarm-specific pingpong
/// protocol (not the libp2p ping protocol).
pub struct PingpongBehaviour {
    /// Configuration for creating handlers.
    config: PingpongConfig,
    /// Pending events to emit.
    events: VecDeque<ToSwarm<PingpongEvent, Command>>,
    /// Map of peer ID to their connection IDs.
    peer_connections: HashMap<PeerId, Vec<ConnectionId>>,
}

impl PingpongBehaviour {
    /// Create a new pingpong behaviour with the given configuration.
    pub fn new(config: PingpongConfig) -> Self {
        Self {
            config,
            events: VecDeque::new(),
            peer_connections: HashMap::new(),
        }
    }

    /// Send a ping to a peer.
    ///
    /// The RTT will be reported via a `PingpongEvent::Pong` event.
    pub fn ping(&mut self, peer_id: PeerId) {
        self.ping_with_greeting(peer_id, None);
    }

    /// Send a ping to a peer with a custom greeting.
    pub fn ping_with_greeting(&mut self, peer_id: PeerId, greeting: Option<String>) {
        if let Some(connections) = self.peer_connections.get(&peer_id) {
            if let Some(&connection_id) = connections.first() {
                self.events.push_back(ToSwarm::NotifyHandler {
                    peer_id,
                    handler: NotifyHandler::One(connection_id),
                    event: Command::Ping { greeting },
                });
            }
        }
    }
}

impl NetworkBehaviour for PingpongBehaviour {
    type ConnectionHandler = Handler;
    type ToSwarm = PingpongEvent;

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(established) => {
                self.peer_connections
                    .entry(established.peer_id)
                    .or_default()
                    .push(established.connection_id);
            }
            FromSwarm::ConnectionClosed(closed) => {
                if let Some(connections) = self.peer_connections.get_mut(&closed.peer_id) {
                    connections.retain(|&id| id != closed.connection_id);
                }
                if closed.remaining_established == 0 {
                    self.peer_connections.remove(&closed.peer_id);
                }
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        _connection_id: ConnectionId,
        event: HandlerEvent,
    ) {
        match event {
            HandlerEvent::Pong { rtt, response: _ } => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(PingpongEvent::Pong { peer_id, rtt }));
            }
            HandlerEvent::PingReceived => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(PingpongEvent::PingReceived {
                        peer_id,
                    }));
            }
            HandlerEvent::Error(error) => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(PingpongEvent::Error {
                        peer_id,
                        error: error.to_string(),
                    }));
            }
        }
    }

    fn poll(
        &mut self,
        _: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }
        Poll::Pending
    }

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(Handler::new(self.config.handler_config.clone()))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(Handler::new(self.config.handler_config.clone()))
    }
}
