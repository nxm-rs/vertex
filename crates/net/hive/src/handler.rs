//! Connection handler for the hive protocol.
//!
//! The handler manages hive streams for a single connection. It:
//! - Handles inbound streams containing peer addresses
//! - Sends outbound peer broadcasts when commanded

use std::{
    collections::VecDeque,
    task::{Context, Poll},
    time::Duration,
};

use libp2p::swarm::{
    handler::{
        ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
        FullyNegotiatedOutbound,
    },
    SubstreamProtocol,
};

use crate::{
    codec::Peers,
    protocol::{HiveError, HiveInboundOutput, HiveInboundProtocol, HiveOutboundProtocol},
};

/// Configuration for the hive handler.
#[derive(Debug, Clone)]
pub struct Config {
    /// Timeout for hive protocol exchange.
    pub timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60), // 1 minute, matching Bee's messageTimeout
        }
    }
}

/// Commands sent from the behaviour to the handler.
#[derive(Debug)]
pub enum Command {
    /// Broadcast peers to the remote.
    BroadcastPeers(Peers),
}

/// Events emitted by the hive handler to the behaviour.
#[derive(Debug)]
pub enum Event {
    /// Received peers from the remote.
    PeersReceived(Peers),
    /// Successfully broadcast peers.
    BroadcastComplete,
    /// Failed to complete a hive operation.
    Error(HiveError),
}

/// Handler for hive protocol on a single connection.
pub struct Handler {
    /// Configuration.
    config: Config,
    /// Pending outbound peer broadcasts.
    pending_outbound: VecDeque<Peers>,
    /// Pending events to emit.
    pending_events: VecDeque<Event>,
}

impl Handler {
    /// Create a new hive handler.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            pending_outbound: VecDeque::new(),
            pending_events: VecDeque::new(),
        }
    }
}

impl ConnectionHandler for Handler {
    type FromBehaviour = Command;
    type ToBehaviour = Event;
    type InboundProtocol = HiveInboundProtocol;
    type OutboundProtocol = HiveOutboundProtocol;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(HiveInboundProtocol::new(), ())
            .with_timeout(self.config.timeout)
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>>
    {
        // Check for pending events first
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Check for pending outbound broadcasts
        if let Some(peers) = self.pending_outbound.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(HiveOutboundProtocol::new(peers), ())
                    .with_timeout(self.config.timeout),
            });
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            Command::BroadcastPeers(peers) => {
                self.pending_outbound.push_back(peers);
            }
        }
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: HiveInboundOutput { peers, .. },
                info: (),
            }) => {
                self.pending_events.push_back(Event::PeersReceived(peers));
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: _,
                info: (),
            }) => {
                self.pending_events.push_back(Event::BroadcastComplete);
            }
            ConnectionEvent::DialUpgradeError(e) => {
                tracing::warn!("Hive dial upgrade error: {:?}", e.error);
                self.pending_events
                    .push_back(Event::Error(HiveError::ConnectionClosed));
            }
            ConnectionEvent::ListenUpgradeError(e) => {
                tracing::warn!("Hive listen upgrade error: {:?}", e.error);
                self.pending_events
                    .push_back(Event::Error(HiveError::ConnectionClosed));
            }
            _ => {}
        }
    }
}
