//! NetworkBehaviour for hive protocol.

use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
};

use crate::HIVE_INBOUND_QUOTA;
use crate::cache::PeerCache;
use crate::handler::{HiveCommand, HiveHandler, HiveHandlerEvent};
use crate::peer_handler::{HivePeerHandler, LearnAndDial};
use libp2p::{
    Multiaddr, PeerId,
    swarm::{
        ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler, THandler, THandlerInEvent,
        THandlerOutEvent, ToSwarm,
    },
};
use strum::IntoStaticStr;
use tracing::debug;
use vertex_net_ratelimiter::KeyedRateLimiter;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_net_headers::ProtocolStreamError;
use vertex_swarm_peer::SwarmPeer;

/// Events emitted by HiveBehaviour.
#[derive(Debug, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum HiveEvent {
    /// Received peers from a connection.
    PeersReceived {
        peer_id: PeerId,
        connection_id: ConnectionId,
        peers: Vec<SwarmPeer>,
    },
    /// Error occurred.
    Error {
        peer_id: PeerId,
        connection_id: ConnectionId,
        error: ProtocolStreamError,
    },
}

/// Behaviour for the Swarm hive protocol.
pub struct HiveBehaviour<I> {
    identity: Arc<I>,
    cache: Arc<PeerCache>,
    events: VecDeque<ToSwarm<HiveEvent, HiveCommand>>,
    /// Cloned into each per-connection handler so the protocol reader can
    /// consult the inbound policy.
    peer_handler: Arc<dyn HivePeerHandler>,
    /// Shared with each handler so per-peer buckets survive reconnects but
    /// are freed by [`Self::on_swarm_event`] on the final `ConnectionClosed`.
    inbound_limit: Arc<KeyedRateLimiter<PeerId>>,
}

impl<I> HiveBehaviour<I>
where
    I: SwarmIdentity + 'static,
{
    /// Construct with the default [`LearnAndDial`] inbound policy.
    pub fn new(identity: Arc<I>) -> Self {
        Self::with_peer_handler(identity, Arc::new(LearnAndDial))
    }

    /// Construct with a custom inbound policy; use [`DiscardSilently`] for
    /// bootnodes.
    ///
    /// [`DiscardSilently`]: crate::DiscardSilently
    pub fn with_peer_handler(identity: Arc<I>, peer_handler: Arc<dyn HivePeerHandler>) -> Self {
        Self {
            identity,
            cache: Arc::new(PeerCache::default()),
            events: VecDeque::new(),
            peer_handler,
            inbound_limit: Arc::new(KeyedRateLimiter::new(HIVE_INBOUND_QUOTA)),
        }
    }

    /// Broadcast a batch of peers on an existing connection. The topology
    /// already throttles broadcast cadence, so no outbound rate-limit is
    /// applied here.
    pub fn broadcast(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        peers: Vec<SwarmPeer>,
    ) {
        if peers.is_empty() {
            return;
        }
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(connection_id),
            event: HiveCommand::BroadcastPeers(peers),
        });
    }
}

impl<I> NetworkBehaviour for HiveBehaviour<I>
where
    I: SwarmIdentity + 'static,
{
    type ConnectionHandler = HiveHandler<I>;
    type ToSwarm = HiveEvent;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, libp2p::swarm::ConnectionDenied> {
        Ok(HiveHandler::new(
            self.identity.clone(),
            peer,
            self.cache.clone(),
            self.inbound_limit.clone(),
            self.peer_handler.clone(),
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _addr: &Multiaddr,
        _role_override: libp2p::core::Endpoint,
        _port_use: libp2p::core::transport::PortUse,
    ) -> Result<THandler<Self>, libp2p::swarm::ConnectionDenied> {
        Ok(HiveHandler::new(
            self.identity.clone(),
            peer,
            self.cache.clone(),
            self.inbound_limit.clone(),
            self.peer_handler.clone(),
        ))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        if let FromSwarm::ConnectionClosed(closed) = event {
            // Free the per-peer rate-limit bucket only when the last
            // connection closes; an earlier clear would let a peer reset its
            // bucket by churning a single connection.
            if closed.remaining_established == 0 {
                self.inbound_limit.clear(&closed.peer_id);
            }
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {
            HiveHandlerEvent::PeersReceived(peers) => {
                // Rate-limit accounting and bootnode-mode discard already
                // happened in the protocol reader. Empty batches reach us
                // when those gates fired or all records failed validation;
                // we have nothing to surface.
                if peers.is_empty() {
                    return;
                }
                self.events
                    .push_back(ToSwarm::GenerateEvent(HiveEvent::PeersReceived {
                        peer_id,
                        connection_id,
                        peers,
                    }));
            }
            HiveHandlerEvent::Error(error) => {
                debug!(%peer_id, %error, "hive error");
                self.events
                    .push_back(ToSwarm::GenerateEvent(HiveEvent::Error {
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

#[cfg(test)]
mod tests {
    use crate::peer_handler::{DiscardSilently, HivePeerHandler, InboundPolicy, LearnAndDial};

    #[test]
    fn discard_silently_returns_discard_policy() {
        assert_eq!(DiscardSilently.policy(), InboundPolicy::Discard);
    }

    #[test]
    fn learn_and_dial_returns_forward_policy() {
        assert_eq!(LearnAndDial.policy(), InboundPolicy::Forward);
    }
}
