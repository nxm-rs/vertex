//! Per-connection handler for pingpong protocol.

use std::{
    collections::VecDeque,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use libp2p::swarm::{
    SubstreamProtocol,
    handler::{
        ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
        FullyNegotiatedOutbound,
    },
};
use tracing::{debug, trace, warn};
use vertex_observability::labels::direction;
use vertex_swarm_net_headers::{ProtocolStreamError, UpgradeError};
use crate::{PingpongInboundProtocol, PingpongOutboundProtocol, inbound, outbound};

/// Configuration for pingpong handler.
#[derive(Debug, Clone)]
pub struct PingpongConfig {
    /// Timeout for pingpong protocol.
    pub timeout: Duration,
    /// Default greeting for pings.
    pub greeting: String,
}

impl Default for PingpongConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            greeting: "ping".to_string(),
        }
    }
}

/// Commands from behaviour to handler.
#[derive(Debug)]
pub enum PingpongHandlerIn {
    /// Send a ping with optional custom greeting.
    Ping { greeting: Option<String> },
}

/// Events from handler to behaviour.
#[derive(Debug)]
pub enum PingpongHandlerOut {
    /// Pong received with RTT.
    Pong {
        /// The pong response string.
        response: String,
        /// Round-trip time.
        rtt: Duration,
    },
    /// Responded to incoming ping.
    PingReceived,
    /// Error occurred.
    Error(ProtocolStreamError),
}

/// Info for tracking outbound requests.
#[derive(Debug, Clone)]
pub struct PingpongOutboundInfo {
    pub sent_at: Instant,
}

/// Per-connection handler for pingpong protocol.
pub struct PingpongHandler {
    config: PingpongConfig,
    pending_events: VecDeque<PingpongHandlerOut>,
    pending_pings: VecDeque<String>,
    outbound_pending: bool,
}

impl PingpongHandler {
    pub fn new(config: PingpongConfig) -> Self {
        Self {
            config,
            pending_events: VecDeque::new(),
            pending_pings: VecDeque::new(),
            outbound_pending: false,
        }
    }
}

impl ConnectionHandler for PingpongHandler {
    type FromBehaviour = PingpongHandlerIn;
    type ToBehaviour = PingpongHandlerOut;
    type InboundProtocol = PingpongInboundProtocol;
    type OutboundProtocol = PingpongOutboundProtocol;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = PingpongOutboundInfo;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(inbound(), ()).with_timeout(self.config.timeout)
    }

    fn connection_keep_alive(&self) -> bool {
        true
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        if !self.outbound_pending {
            if let Some(greeting) = self.pending_pings.pop_front() {
                self.outbound_pending = true;
                let sent_at = Instant::now();
                debug!(%greeting, "Sending ping");
                return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                    protocol: SubstreamProtocol::new(
                        outbound(greeting),
                        PingpongOutboundInfo { sent_at },
                    )
                    .with_timeout(self.config.timeout),
                });
            }
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            PingpongHandlerIn::Ping { greeting } => {
                let greeting = greeting.unwrap_or_else(|| self.config.greeting.clone());
                self.pending_pings.push_back(greeting);
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
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound { .. }) => {
                trace!("Responded to ping");
                self.pending_events
                    .push_back(PingpongHandlerOut::PingReceived);
            }

            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: response,
                info,
                ..
            }) => {
                self.outbound_pending = false;
                let rtt = info.sent_at.elapsed();
                trace!(?rtt, "Pong received");
                self.pending_events
                    .push_back(PingpongHandlerOut::Pong { response, rtt });
            }

            ConnectionEvent::DialUpgradeError(error) => {
                self.outbound_pending = false;
                let upgrade_error = UpgradeError::from(error.error);
                upgrade_error.record("pingpong", direction::OUTBOUND);
                let pingpong_error = ProtocolStreamError::from(upgrade_error);
                warn!(error = %pingpong_error, "Pingpong outbound error");
                self.pending_events
                    .push_back(PingpongHandlerOut::Error(pingpong_error));
            }

            ConnectionEvent::ListenUpgradeError(error) => {
                let upgrade_error = UpgradeError::from(error.error);
                upgrade_error.record("pingpong", direction::INBOUND);
                let pingpong_error = ProtocolStreamError::from(upgrade_error);
                warn!(error = %pingpong_error, "Pingpong inbound error");
                self.pending_events
                    .push_back(PingpongHandlerOut::Error(pingpong_error));
            }

            _ => {}
        }
    }
}
