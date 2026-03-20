//! Per-connection handler for hive protocol.
//!
//! Inbound substreams use `Inbound<HiveInner>` as the `InboundProtocol`,
//! so libp2p runs the full upgrade (headers exchange + protobuf recv +
//! peer validation). The handler receives `ValidatedPeers` directly in
//! `FullyNegotiatedInbound`.

use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use libp2p::{
    PeerId,
    swarm::{
        SubstreamProtocol,
        handler::{
            ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
            FullyNegotiatedOutbound, ListenUpgradeError,
        },
    },
};
use metrics::counter;
use tracing::{debug, trace, warn};
use vertex_observability::labels::direction;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_net_handler_core::HandlerCore;
use vertex_swarm_net_headers::{Inbound, Outbound, ProtocolStreamError, UpgradeError};
use vertex_swarm_peer::SwarmPeer;

use crate::protocol::{HiveInner, HiveOutboundInner, HiveOutboundProtocol, PeerCache};

/// Timeout for stream processing (headers exchange + protobuf + validation).
const STREAM_TIMEOUT: Duration = Duration::from_secs(10);

/// Rate limiter: burst of 4 streams, refill 1 token every 5 seconds.
/// Sustains ~12 inbound streams per minute, which is generous for topology updates.
const RATE_LIMIT_BURST: u32 = 4;
const RATE_LIMIT_REFILL: Duration = Duration::from_secs(5);

/// Maximum queued broadcasts before dropping (prevents unbounded growth if outbound is slow).
const MAX_PENDING_BROADCASTS: usize = 16;

/// Commands from behaviour to handler.
#[derive(Debug)]
pub enum HiveCommand {
    /// Broadcast peers to this connection.
    BroadcastPeers(Vec<SwarmPeer>),
}

/// Events from handler to behaviour.
#[derive(Debug)]
pub enum HiveHandlerEvent {
    /// Received peers from this connection.
    PeersReceived(Vec<SwarmPeer>),
    /// Error occurred.
    Error(ProtocolStreamError),
}

/// Inbound protocol type: full headers exchange + protobuf recv + validation.
pub(crate) type HiveInboundProtocol<I> = Inbound<HiveInner<I>>;

/// Per-connection handler for hive protocol.
pub struct HiveHandler<I: SwarmIdentity> {
    remote_peer_id: PeerId,
    identity: Arc<I>,
    cache: PeerCache,
    /// Shared handler core: events, rate limiter, outbound flag.
    core: HandlerCore<HiveHandlerEvent>,
    /// Bounded by [`MAX_PENDING_BROADCASTS`]; excess broadcasts are dropped with a warning.
    pending_broadcasts: VecDeque<Vec<SwarmPeer>>,
}

impl<I> HiveHandler<I>
where
    I: SwarmIdentity + 'static,
{
    /// Create a new hive handler.
    pub(crate) fn new(identity: Arc<I>, remote_peer_id: PeerId, cache: PeerCache) -> Self {
        Self {
            core: HandlerCore::new(RATE_LIMIT_BURST, RATE_LIMIT_REFILL),
            remote_peer_id,
            identity,
            cache,
            pending_broadcasts: VecDeque::new(),
        }
    }
}

impl<I> ConnectionHandler for HiveHandler<I>
where
    I: SwarmIdentity + 'static,
{
    type FromBehaviour = HiveCommand;
    type ToBehaviour = HiveHandlerEvent;
    type InboundProtocol = HiveInboundProtocol<I>;
    type OutboundProtocol = HiveOutboundProtocol;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        let inner = HiveInner::new(self.identity.clone(), self.cache.clone());
        let upgrade = Inbound::new(inner);
        SubstreamProtocol::new(upgrade, ()).with_timeout(STREAM_TIMEOUT)
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
        // Emit pending events
        if let Some(event) = self.core.poll_pending() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Process pending broadcasts
        if !self.core.outbound_pending()
            && let Some(peers) = self.pending_broadcasts.pop_front()
        {
            self.core.set_outbound_pending(true);
            debug!(peer_count = peers.len(), "Sending hive broadcast");
            let protocol = Outbound::new(HiveOutboundInner::new(&peers));
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(protocol, ()).with_timeout(STREAM_TIMEOUT),
            });
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            HiveCommand::BroadcastPeers(peers) => {
                if self.pending_broadcasts.len() >= MAX_PENDING_BROADCASTS {
                    warn!(
                        peer_id = %self.remote_peer_id,
                        queue_len = self.pending_broadcasts.len(),
                        "Dropping hive broadcast: pending queue full"
                    );
                    return;
                }
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
                protocol: validated,
                ..
            }) => {
                if !self.core.try_accept_inbound() {
                    warn!(
                        peer_id = %self.remote_peer_id,
                        "Rate limiting inbound hive stream - dropping result"
                    );
                    counter!("hive_rate_limited_total").increment(1);
                    return;
                }
                self.core
                    .push_event(HiveHandlerEvent::PeersReceived(validated.peers));
            }

            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound { .. }) => {
                self.core.set_outbound_pending(false);
                trace!("Hive broadcast completed");
            }

            ConnectionEvent::ListenUpgradeError(ListenUpgradeError { error, .. }) => {
                let hive_error =
                    UpgradeError::record_and_convert(error, "hive", direction::INBOUND);
                warn!(error = %hive_error, "Hive inbound stream error");
                self.core.push_event(HiveHandlerEvent::Error(hive_error));
            }

            ConnectionEvent::DialUpgradeError(error) => {
                self.core.set_outbound_pending(false);
                let hive_error =
                    UpgradeError::record_and_convert(error.error, "hive", direction::OUTBOUND);
                warn!(error = %hive_error, "Hive outbound error");
                self.core.push_event(HiveHandlerEvent::Error(hive_error));
            }

            _ => {}
        }
    }
}
