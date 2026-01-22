//! NetworkBehaviour implementation for the hive protocol.
//!
//! This behaviour handles peer discovery gossip for the Swarm network.
//! It allows broadcasting known peers to other nodes and receiving
//! peer announcements from the network.

use std::{
    collections::{HashMap, VecDeque},
    task::{Context, Poll},
};

use libp2p::{
    core::{transport::PortUse, Endpoint},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler,
        THandlerInEvent, ToSwarm,
    },
    Multiaddr, PeerId,
};

use crate::{
    codec::{BzzAddress, Peers},
    handler::{Command, Config, Event as HandlerEvent, Handler},
};

/// Maximum number of peers per broadcast message (matching Bee's maxBatchSize).
pub const MAX_BATCH_SIZE: usize = 30;

/// Events emitted by the hive behaviour.
#[derive(Debug)]
pub enum HiveEvent {
    /// Received peers from a remote node.
    PeersReceived {
        /// The peer that sent the announcement.
        from: PeerId,
        /// The peers that were announced.
        peers: Vec<BzzAddress>,
    },
    /// Successfully broadcast peers to a node.
    BroadcastComplete {
        /// The peer we broadcast to.
        to: PeerId,
    },
    /// An error occurred.
    Error {
        /// The peer involved.
        peer_id: PeerId,
        /// Error description.
        error: String,
    },
}

/// Configuration for the hive behaviour.
#[derive(Debug, Clone)]
pub struct HiveConfig {
    /// Handler configuration.
    pub handler_config: Config,
}

impl Default for HiveConfig {
    fn default() -> Self {
        Self {
            handler_config: Config::default(),
        }
    }
}

/// NetworkBehaviour for the hive protocol.
///
/// This behaviour handles peer discovery by:
/// - Receiving peer announcements from connected nodes
/// - Broadcasting known peers to other nodes when requested
pub struct HiveBehaviour {
    /// Configuration for creating handlers.
    config: HiveConfig,
    /// Pending events to emit.
    events: VecDeque<ToSwarm<HiveEvent, Command>>,
    /// Map of peer ID to their connection IDs.
    peer_connections: HashMap<PeerId, Vec<ConnectionId>>,
}

impl HiveBehaviour {
    /// Create a new hive behaviour with the given configuration.
    pub fn new(config: HiveConfig) -> Self {
        Self {
            config,
            events: VecDeque::new(),
            peer_connections: HashMap::new(),
        }
    }

    /// Broadcast peers to a specific node.
    ///
    /// The peers are split into batches of at most `MAX_BATCH_SIZE` peers.
    /// Each batch is sent as a separate message.
    ///
    /// # Arguments
    /// * `to` - The peer to broadcast to
    /// * `peers` - The peers to announce
    pub fn broadcast_peers(&mut self, to: PeerId, peers: Vec<BzzAddress>) {
        if peers.is_empty() {
            return;
        }

        // Get connections for this peer
        let connections = match self.peer_connections.get(&to) {
            Some(conns) if !conns.is_empty() => conns,
            _ => {
                tracing::warn!(%to, "Cannot broadcast peers: no connections");
                return;
            }
        };

        // Use the first connection
        let connection_id = connections[0];

        // Split into batches and send
        for chunk in peers.chunks(MAX_BATCH_SIZE) {
            let batch = Peers::new(chunk.to_vec());
            self.events.push_back(ToSwarm::NotifyHandler {
                peer_id: to,
                handler: NotifyHandler::One(connection_id),
                event: Command::BroadcastPeers(batch),
            });
        }
    }

    /// Broadcast a single batch of peers to a specific node.
    ///
    /// Unlike `broadcast_peers`, this does not split the peers into batches.
    /// The caller is responsible for respecting `MAX_BATCH_SIZE`.
    pub fn broadcast_peers_batch(&mut self, to: PeerId, peers: Peers) {
        // Get connections for this peer
        let connections = match self.peer_connections.get(&to) {
            Some(conns) if !conns.is_empty() => conns,
            _ => {
                tracing::warn!(%to, "Cannot broadcast peers: no connections");
                return;
            }
        };

        // Use the first connection
        let connection_id = connections[0];

        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id: to,
            handler: NotifyHandler::One(connection_id),
            event: Command::BroadcastPeers(peers),
        });
    }

    /// Check if we have a connection to a peer.
    pub fn is_connected(&self, peer: &PeerId) -> bool {
        self.peer_connections
            .get(peer)
            .map(|conns| !conns.is_empty())
            .unwrap_or(false)
    }

    /// Get the list of connected peers.
    pub fn connected_peers(&self) -> impl Iterator<Item = &PeerId> {
        self.peer_connections
            .iter()
            .filter(|(_, conns)| !conns.is_empty())
            .map(|(peer, _)| peer)
    }
}

impl NetworkBehaviour for HiveBehaviour {
    type ConnectionHandler = Handler;
    type ToSwarm = HiveEvent;

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
            HandlerEvent::PeersReceived(peers) => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(HiveEvent::PeersReceived {
                        from: peer_id,
                        peers: peers.peers,
                    }));
            }
            HandlerEvent::BroadcastComplete => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(HiveEvent::BroadcastComplete {
                        to: peer_id,
                    }));
            }
            HandlerEvent::Error(error) => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(HiveEvent::Error {
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
