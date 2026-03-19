//! Per-connection handler for pingpong protocol.
//!
//! Uses `FuturesSet` for bounded inbound stream concurrency, following the
//! same pattern as the identify and hive handlers. Inbound streams are accepted
//! via `ReadyUpgrade` (protocol negotiation only), then the actual protocol
//! work (headers exchange + ping recv + pong send) runs inside the bounded set
//! with a per-stream timeout.

use std::{
    collections::VecDeque,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use futures_bounded::Timeout;
use libp2p::{
    InboundUpgrade, PeerId,
    core::upgrade::ReadyUpgrade,
    swarm::{
        StreamProtocol, SubstreamProtocol,
        handler::{
            ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
            FullyNegotiatedOutbound,
        },
    },
};
use metrics::counter;
use tracing::{debug, trace, warn};
use vertex_net_ratelimiter::RateLimiter;
use vertex_observability::labels::direction;
use vertex_swarm_net_headers::{Inbound, ProtocolError, ProtocolStreamError, UpgradeError};

use crate::{PROTOCOL_NAME, PingpongOutboundProtocol, outbound, protocol::PingpongInboundInner};

/// Timeout for inbound stream processing (headers exchange + ping/pong).
const STREAM_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum concurrent inbound pingpong streams per connection.
const MAX_CONCURRENT_STREAMS_PER_CONNECTION: usize = 2;

/// Rate limiter: burst of 2 streams, refill 1 token every 2 seconds.
/// Sustains ~30 inbound pings per minute, generous for liveness checks.
const RATE_LIMIT_BURST: u32 = 2;
const RATE_LIMIT_REFILL: Duration = Duration::from_secs(2);

/// Maximum queued outbound ping commands per connection.
const MAX_PENDING_PINGS: usize = 8;

/// StreamProtocol for multistream-select negotiation.
const PINGPONG_STREAM_PROTOCOL: StreamProtocol = StreamProtocol::new(PROTOCOL_NAME);

/// Configuration for pingpong handler.
#[derive(Debug, Clone)]
pub struct PingpongConfig {
    /// Timeout for pingpong outbound protocol.
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
pub enum PingpongCommand {
    /// Send a ping with optional custom greeting.
    Ping { greeting: Option<String> },
}

/// Events from handler to behaviour.
#[derive(Debug)]
pub enum PingpongHandlerEvent {
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
    remote_peer_id: PeerId,
    /// Bounded set of active inbound streams for backpressure.
    active_streams: futures_bounded::FuturesSet<Result<(), ProtocolError>>,
    /// Token bucket to prevent rapid stream cycling.
    rate_limiter: RateLimiter,
    /// Implicitly bounded by `FuturesSet` max capacity + one event per outbound completion/error.
    pending_events: VecDeque<PingpongHandlerEvent>,
    /// Bounded by `MAX_PENDING_PINGS`; excess commands are dropped with a warning.
    pending_pings: VecDeque<String>,
    outbound_pending: bool,
}

impl PingpongHandler {
    pub fn new(config: PingpongConfig, remote_peer_id: PeerId) -> Self {
        Self {
            active_streams: futures_bounded::FuturesSet::new(
                STREAM_TIMEOUT,
                MAX_CONCURRENT_STREAMS_PER_CONNECTION,
            ),
            rate_limiter: RateLimiter::new(RATE_LIMIT_BURST, RATE_LIMIT_REFILL),
            config,
            remote_peer_id,
            pending_events: VecDeque::new(),
            pending_pings: VecDeque::new(),
            outbound_pending: false,
        }
    }
}

impl ConnectionHandler for PingpongHandler {
    type FromBehaviour = PingpongCommand;
    type ToBehaviour = PingpongHandlerEvent;
    type InboundProtocol = ReadyUpgrade<StreamProtocol>;
    type OutboundProtocol = PingpongOutboundProtocol;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = PingpongOutboundInfo;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(ReadyUpgrade::new(PINGPONG_STREAM_PROTOCOL), ())
    }

    fn connection_keep_alive(&self) -> bool {
        true
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Drain completed inbound streams (one per poll to yield between events).
        if let Poll::Ready(result) = self.active_streams.poll_unpin(cx) {
            match result {
                Ok(Ok(())) => {
                    trace!("Responded to ping");
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        PingpongHandlerEvent::PingReceived,
                    ));
                }
                Ok(Err(err)) => {
                    let pingpong_error =
                        UpgradeError::record_and_convert(err, "pingpong", direction::INBOUND);
                    warn!(error = %pingpong_error, "Pingpong inbound stream error");
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        PingpongHandlerEvent::Error(pingpong_error),
                    ));
                }
                Err(Timeout { .. }) => {
                    warn!("Pingpong inbound stream timed out");
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        PingpongHandlerEvent::Error(ProtocolStreamError::Timeout),
                    ));
                }
            }
        }

        if !self.outbound_pending
            && let Some(greeting) = self.pending_pings.pop_front()
        {
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

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            PingpongCommand::Ping { greeting } => {
                if self.pending_pings.len() >= MAX_PENDING_PINGS {
                    warn!(
                        peer_id = %self.remote_peer_id,
                        max = MAX_PENDING_PINGS,
                        "Dropping ping command: pending queue full"
                    );
                    return;
                }
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
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: stream,
                ..
            }) => {
                if !self.rate_limiter.try_acquire() {
                    warn!(peer_id = %self.remote_peer_id, "Rate limiting inbound pingpong stream");
                    counter!("pingpong_rate_limited_total").increment(1);
                    return;
                }
                if self
                    .active_streams
                    .try_push(async move {
                        let upgrade = Inbound::new(PingpongInboundInner);
                        upgrade.upgrade_inbound(stream, PROTOCOL_NAME).await
                    })
                    .is_err()
                {
                    warn!("Dropping inbound pingpong stream because we are at capacity");
                }
            }

            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: response,
                info,
                ..
            }) => {
                self.outbound_pending = false;
                // Handler-level RTT: substream request → upgrade completion (includes
                // protocol negotiation + headers exchange). The protocol-level RTT in
                // protocol.rs measures only ping-send → pong-receive within the stream.
                let rtt = info.sent_at.elapsed();
                trace!(?rtt, "Pong received");
                self.pending_events
                    .push_back(PingpongHandlerEvent::Pong { response, rtt });
            }

            ConnectionEvent::DialUpgradeError(error) => {
                self.outbound_pending = false;
                let pingpong_error =
                    UpgradeError::record_and_convert(error.error, "pingpong", direction::OUTBOUND);
                warn!(error = %pingpong_error, "Pingpong outbound error");
                self.pending_events
                    .push_back(PingpongHandlerEvent::Error(pingpong_error));
            }

            _ => {}
        }
    }
}
