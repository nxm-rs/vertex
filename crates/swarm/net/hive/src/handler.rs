//! Per-connection handler for hive protocol.
//!
//! Uses `FuturesSet` for bounded inbound stream concurrency, following the
//! same pattern as the identify handler. Inbound streams are accepted via
//! `ReadyUpgrade` (protocol negotiation only), then the actual protocol work
//! (headers exchange + protobuf recv + peer validation) runs inside the
//! bounded set with a per-stream timeout.

use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
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
use vertex_observability::labels::direction;
use vertex_swarm_api::SwarmIdentity;
use vertex_net_ratelimiter::RateLimiter;
use vertex_swarm_net_headers::{Inbound, ProtocolError, ProtocolStreamError, UpgradeError};
use vertex_swarm_peer::SwarmPeer;

use crate::{HiveOutboundProtocol, PROTOCOL_NAME, outbound, protocol::HiveInner};

/// Timeout for inbound stream processing (headers exchange + recv + validation).
const STREAM_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum concurrent inbound hive streams per connection.
const MAX_CONCURRENT_STREAMS_PER_CONNECTION: usize = 4;

/// Rate limiter: burst of 4 streams, refill 1 token every 5 seconds.
/// Sustains ~12 inbound streams per minute, which is generous for topology updates.
const RATE_LIMIT_BURST: u32 = 4;
const RATE_LIMIT_REFILL: Duration = Duration::from_secs(5);

/// StreamProtocol for multistream-select negotiation.
const HIVE_STREAM_PROTOCOL: StreamProtocol = StreamProtocol::new(PROTOCOL_NAME);

/// Configuration for hive handler.
#[derive(Debug, Clone)]
pub struct HiveConfig {
    /// Timeout for hive outbound protocol.
    pub timeout: Duration,
}

impl Default for HiveConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
        }
    }
}

/// Commands from behaviour to handler.
#[derive(Debug)]
pub enum HiveHandlerIn {
    /// Broadcast peers to this connection.
    BroadcastPeers(Vec<SwarmPeer>),
}

/// Events from handler to behaviour.
#[derive(Debug)]
pub enum HiveHandlerOut {
    /// Received peers from this connection.
    PeersReceived(Vec<SwarmPeer>),
    /// Broadcast completed successfully.
    BroadcastComplete,
    /// Error occurred.
    Error(ProtocolStreamError),
}

/// Per-connection handler for hive protocol.
pub struct HiveHandler<I> {
    config: HiveConfig,
    remote_peer_id: PeerId,
    identity: Arc<I>,
    /// Bounded set of active inbound streams for backpressure.
    active_streams: futures_bounded::FuturesSet<Result<Vec<SwarmPeer>, ProtocolError>>,
    /// Token bucket to prevent rapid stream cycling.
    rate_limiter: RateLimiter,
    pending_events: VecDeque<HiveHandlerOut>,
    pending_broadcasts: VecDeque<Vec<SwarmPeer>>,
    outbound_pending: bool,
}

impl<I> HiveHandler<I>
where
    I: SwarmIdentity + 'static,
{
    /// Create a new hive handler.
    pub fn new(config: HiveConfig, identity: Arc<I>, remote_peer_id: PeerId) -> Self {
        Self {
            active_streams: futures_bounded::FuturesSet::new(
                STREAM_TIMEOUT,
                MAX_CONCURRENT_STREAMS_PER_CONNECTION,
            ),
            rate_limiter: RateLimiter::new(RATE_LIMIT_BURST, RATE_LIMIT_REFILL),
            config,
            remote_peer_id,
            identity,
            pending_events: VecDeque::new(),
            pending_broadcasts: VecDeque::new(),
            outbound_pending: false,
        }
    }
}

impl<I> ConnectionHandler for HiveHandler<I>
where
    I: SwarmIdentity + 'static,
{
    type FromBehaviour = HiveHandlerIn;
    type ToBehaviour = HiveHandlerOut;
    type InboundProtocol = ReadyUpgrade<StreamProtocol>;
    type OutboundProtocol = HiveOutboundProtocol;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(ReadyUpgrade::new(HIVE_STREAM_PROTOCOL), ())
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
        // Emit pending events
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Drain completed inbound streams
        while let Poll::Ready(result) = self.active_streams.poll_unpin(cx) {
            match result {
                Ok(Ok(peers)) => {
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        HiveHandlerOut::PeersReceived(peers),
                    ));
                }
                Ok(Err(err)) => {
                    let upgrade_error = UpgradeError::from(err);
                    upgrade_error.record_if_untracked("hive", direction::INBOUND);
                    let hive_error = ProtocolStreamError::from(upgrade_error);
                    warn!(error = %hive_error, "Hive inbound stream error");
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        HiveHandlerOut::Error(hive_error),
                    ));
                }
                Err(Timeout { .. }) => {
                    warn!("Hive inbound stream timed out");
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        HiveHandlerOut::Error(ProtocolStreamError::Timeout),
                    ));
                }
            }
        }

        // Process pending broadcasts
        if !self.outbound_pending {
            if let Some(peers) = self.pending_broadcasts.pop_front() {
                self.outbound_pending = true;
                debug!(peer_count = peers.len(), "Sending hive broadcast");
                return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                    protocol: SubstreamProtocol::new(outbound(&peers), ())
                        .with_timeout(self.config.timeout),
                });
            }
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            HiveHandlerIn::BroadcastPeers(peers) => {
                self.pending_broadcasts.push_back(peers);
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
                    warn!(peer_id = %self.remote_peer_id, "Rate limiting inbound hive stream");
                    counter!("hive_rate_limited_total").increment(1);
                    return;
                }
                let identity = self.identity.clone();
                if self
                    .active_streams
                    .try_push(async move {
                        let upgrade = Inbound::new(HiveInner::new(identity));
                        upgrade
                            .upgrade_inbound(stream, PROTOCOL_NAME)
                            .await
                            .map(|v| v.peers)
                    })
                    .is_err()
                {
                    warn!("Dropping inbound hive stream because we are at capacity");
                }
            }

            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound { .. }) => {
                self.outbound_pending = false;
                trace!("Hive broadcast completed");
                self.pending_events.push_back(HiveHandlerOut::BroadcastComplete);
            }

            ConnectionEvent::DialUpgradeError(error) => {
                self.outbound_pending = false;
                let upgrade_error = UpgradeError::from(error.error);
                upgrade_error.record_if_untracked("hive", direction::OUTBOUND);
                let hive_error = ProtocolStreamError::from(upgrade_error);
                warn!(error = %hive_error, "Hive outbound error");
                self.pending_events
                    .push_back(HiveHandlerOut::Error(hive_error));
            }

            _ => {}
        }
    }
}
