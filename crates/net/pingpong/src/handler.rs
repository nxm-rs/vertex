//! Connection handler for the pingpong protocol.
//!
//! This handler:
//! - Responds to incoming ping requests with pongs (automatic)
//! - Initiates outbound pings when commanded

use std::{
    collections::VecDeque,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use libp2p::swarm::{
    handler::{
        ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
        FullyNegotiatedOutbound,
    },
    SubstreamProtocol,
};
use tracing::{debug, warn};
use vertex_net_headers::ProtocolError;

use crate::protocol::{self, PingpongInboundProtocol, PingpongOutboundProtocol};

/// Configuration for the pingpong handler.
#[derive(Debug, Clone)]
pub struct Config {
    /// Timeout for pingpong protocol exchange.
    pub timeout: Duration,
    /// Default greeting for pings.
    pub greeting: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            greeting: "ping".to_string(),
        }
    }
}

/// Commands sent from the behaviour to the handler.
#[derive(Debug)]
pub enum Command {
    /// Send a ping to the peer.
    Ping {
        /// Optional custom greeting (uses config default if None).
        greeting: Option<String>,
    },
}

/// Events emitted by the pingpong handler to the behaviour.
#[derive(Debug)]
pub enum Event {
    /// Successfully received a pong response.
    Pong {
        /// The round-trip time.
        rtt: Duration,
        /// The response message.
        response: String,
    },
    /// Responded to an incoming ping.
    PingReceived,
    /// Failed to complete the pingpong exchange.
    Error(ProtocolError),
}

/// Handler state for tracking outbound pings.
struct PendingPing {
    /// When the ping was sent.
    sent_at: Instant,
}

/// Handler for pingpong protocol on a single connection.
pub struct Handler {
    /// Configuration.
    config: Config,
    /// Pending outbound ping (only one at a time).
    pending_ping: Option<PendingPing>,
    /// Whether an outbound request is in flight.
    outbound_pending: bool,
    /// Pending events to emit.
    pending_events: VecDeque<Event>,
    /// Pending commands to process.
    pending_commands: VecDeque<Command>,
}

impl Handler {
    /// Create a new pingpong handler.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            pending_ping: None,
            outbound_pending: false,
            pending_events: VecDeque::new(),
            pending_commands: VecDeque::new(),
        }
    }
}

impl ConnectionHandler for Handler {
    type FromBehaviour = Command;
    type ToBehaviour = Event;
    type InboundProtocol = PingpongInboundProtocol;
    type OutboundProtocol = PingpongOutboundProtocol;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(protocol::inbound(), ()).with_timeout(self.config.timeout)
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

        // Process pending commands
        while let Some(cmd) = self.pending_commands.pop_front() {
            match cmd {
                Command::Ping { greeting } => {
                    if !self.outbound_pending {
                        self.outbound_pending = true;
                        self.pending_ping = Some(PendingPing {
                            sent_at: Instant::now(),
                        });
                        let greeting = greeting.unwrap_or_else(|| self.config.greeting.clone());
                        return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                            protocol: SubstreamProtocol::new(protocol::outbound(greeting), ())
                                .with_timeout(self.config.timeout),
                        });
                    } else {
                        debug!("Pingpong: Outbound already pending, ignoring");
                    }
                }
            }
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        self.pending_commands.push_back(event);
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
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound { .. }) => {
                // Inbound ping was handled (pong sent automatically)
                debug!("Pingpong: Responded to incoming ping");
                self.pending_events.push_back(Event::PingReceived);
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: pong, ..
            }) => {
                self.outbound_pending = false;
                if let Some(pending) = self.pending_ping.take() {
                    let rtt = pending.sent_at.elapsed();
                    debug!(?rtt, response = %pong.response, "Pingpong: Received pong");
                    self.pending_events.push_back(Event::Pong {
                        rtt,
                        response: pong.response,
                    });
                }
            }
            ConnectionEvent::DialUpgradeError(e) => {
                self.outbound_pending = false;
                self.pending_ping = None;
                warn!("Pingpong dial upgrade error: {:?}", e.error);
                if let libp2p::swarm::StreamUpgradeError::Apply(err) = e.error {
                    self.pending_events.push_back(Event::Error(err));
                }
            }
            ConnectionEvent::ListenUpgradeError(e) => {
                warn!("Pingpong listen upgrade error: {:?}", e.error);
                self.pending_events.push_back(Event::Error(e.error));
            }
            _ => {}
        }
    }
}
