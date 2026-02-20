//! Per-connection handler for hive protocol.

use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
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
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_net_headers::{ProtocolStreamError, UpgradeError};
use vertex_swarm_peer::SwarmPeer;

use crate::{HiveInboundProtocol, HiveOutboundProtocol, inbound, outbound};

/// Configuration for hive handler.
#[derive(Debug, Clone)]
pub struct HiveConfig {
    /// Timeout for hive protocol.
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
    identity: Arc<I>,
    pending_events: VecDeque<HiveHandlerOut>,
    pending_broadcasts: VecDeque<Vec<SwarmPeer>>,
    outbound_pending: bool,
}

impl<I> HiveHandler<I>
where
    I: SwarmIdentity + 'static,
{
    /// Create a new hive handler.
    pub fn new(config: HiveConfig, identity: Arc<I>) -> Self {
        Self {
            config,
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
    type InboundProtocol = HiveInboundProtocol<I>;
    type OutboundProtocol = HiveOutboundProtocol;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(inbound(self.identity.clone()), ())
            .with_timeout(self.config.timeout)
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
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
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
                protocol: validated,
                ..
            }) => {
                debug!(peer_count = validated.peers.len(), "Received hive peers");
                self.pending_events
                    .push_back(HiveHandlerOut::PeersReceived(validated.peers));
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

            ConnectionEvent::ListenUpgradeError(error) => {
                let upgrade_error = UpgradeError::from(error.error);
                upgrade_error.record_if_untracked("hive", direction::INBOUND);
                let hive_error = ProtocolStreamError::from(upgrade_error);
                warn!(error = %hive_error, "Hive inbound error");
                self.pending_events
                    .push_back(HiveHandlerOut::Error(hive_error));
            }

            _ => {}
        }
    }
}
