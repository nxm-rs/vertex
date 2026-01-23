//! SwarmClientBehaviour for managing client-side protocols.
//!
//! This behaviour manages multiple client protocols (pricing, retrieval, pushsync)
//! using a per-connection handler pattern. Handlers start in dormant state and are
//! activated after handshake completion.

use std::{
    collections::{HashMap, VecDeque},
    task::{Context, Poll},
};

use libp2p::{
    Multiaddr, PeerId,
    core::Endpoint,
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
        THandlerOutEvent, ToSwarm,
    },
};
use tracing::{debug, trace};
use vertex_primitives::OverlayAddress;

use crate::{
    ClientCommand, ClientEvent,
    handler::{Config as HandlerConfig, HandlerCommand, HandlerEvent, SwarmClientHandler},
};

/// Configuration for the client behaviour.
#[derive(Debug, Clone)]
pub struct Config {
    /// Handler configuration.
    pub handler: HandlerConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            handler: HandlerConfig::default(),
        }
    }
}

/// The SwarmClientBehaviour manages client-side protocols.
///
/// It creates handlers in dormant state for each connection, and activates
/// them after receiving `ActivatePeer` commands (typically sent after
/// handshake completion).
pub struct SwarmClientBehaviour {
    config: Config,
    /// Map of peer_id -> overlay for activated peers.
    peer_overlays: HashMap<PeerId, OverlayAddress>,
    /// Map of overlay -> peer_id for reverse lookup.
    overlay_peers: HashMap<OverlayAddress, PeerId>,
    /// Pending events to emit.
    pending_events: VecDeque<ToSwarm<ClientEvent, HandlerCommand>>,
}

impl SwarmClientBehaviour {
    /// Create a new client behaviour with the given configuration.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            peer_overlays: HashMap::new(),
            overlay_peers: HashMap::new(),
            pending_events: VecDeque::new(),
        }
    }

    /// Handle a command from the application layer.
    pub fn on_command(&mut self, command: ClientCommand) {
        match command {
            ClientCommand::ActivatePeer {
                peer_id,
                overlay,
                is_full_node,
            } => {
                debug!(%peer_id, %overlay, %is_full_node, "Activating peer");
                self.peer_overlays.insert(peer_id, overlay);
                self.overlay_peers.insert(overlay, peer_id);
                self.pending_events.push_back(ToSwarm::NotifyHandler {
                    peer_id,
                    handler: libp2p::swarm::NotifyHandler::Any,
                    event: HandlerCommand::Activate {
                        overlay,
                        is_full_node,
                    },
                });
            }
            ClientCommand::AnnouncePricing { peer, threshold } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %threshold, "Announcing pricing");
                    self.pending_events.push_back(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::AnnouncePricing { threshold },
                    });
                } else {
                    debug!(%peer, "Unknown peer for pricing announcement");
                }
            }
            ClientCommand::RetrieveChunk { peer, address } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %address, "Retrieving chunk");
                    self.pending_events.push_back(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::RetrieveChunk { address },
                    });
                } else {
                    debug!(%peer, "Unknown peer for retrieval");
                }
            }
            ClientCommand::PushChunk {
                peer,
                address,
                data,
                stamp,
            } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %address, "Pushing chunk");
                    self.pending_events.push_back(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::PushChunk {
                            address,
                            data,
                            stamp,
                        },
                    });
                } else {
                    debug!(%peer, "Unknown peer for push");
                }
            }
            ClientCommand::ServeChunk {
                peer,
                address: _,
                data: _,
                stamp: _,
            } => {
                // TODO: Implement serving chunks (responding to retrieval requests)
                if self.overlay_peers.get(&peer).is_some() {
                    trace!("ServeChunk not yet implemented");
                }
            }
            ClientCommand::SendReceipt {
                peer,
                address: _,
                signature: _,
                nonce: _,
                storage_radius: _,
            } => {
                // TODO: Implement sending receipts
                if self.overlay_peers.get(&peer).is_some() {
                    trace!("SendReceipt not yet implemented");
                }
            }
            ClientCommand::SendCheque { peer, cheque: _ } => {
                // TODO: Implement sending cheques
                if self.overlay_peers.get(&peer).is_some() {
                    trace!("SendCheque not yet implemented");
                }
            }
            ClientCommand::DisconnectPeer { peer, reason } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, ?reason, "Disconnecting peer");
                    self.pending_events.push_back(ToSwarm::CloseConnection {
                        peer_id,
                        connection: libp2p::swarm::CloseConnection::All,
                    });
                }
            }
        }
    }

    /// Handle an event from a handler.
    fn on_handler_event(&mut self, peer_id: PeerId, event: HandlerEvent) {
        match event {
            HandlerEvent::Activated { overlay } => {
                debug!(%peer_id, %overlay, "Handler activated");
                // Already tracked in on_command, just emit event
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(ClientEvent::PeerActivated {
                        peer_id,
                        overlay,
                    }));
            }
            HandlerEvent::PricingReceived { overlay, threshold } => {
                self.pending_events.push_back(ToSwarm::GenerateEvent(
                    ClientEvent::PricingReceived {
                        peer: overlay,
                        peer_id,
                        threshold,
                    },
                ));
            }
            HandlerEvent::PricingSent { overlay } => {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(ClientEvent::PricingSent {
                        peer: overlay,
                    }));
            }
            HandlerEvent::ChunkRequested {
                overlay,
                address,
                request_id,
            } => {
                self.pending_events.push_back(ToSwarm::GenerateEvent(
                    ClientEvent::ChunkRequested {
                        peer: overlay,
                        peer_id,
                        address,
                        request_id,
                    },
                ));
            }
            HandlerEvent::ChunkReceived {
                overlay,
                address,
                data,
                stamp,
            } => {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(ClientEvent::ChunkReceived {
                        peer: overlay,
                        address,
                        data,
                        stamp,
                    }));
            }
            HandlerEvent::ChunkPushReceived {
                overlay,
                address,
                data,
                stamp,
                request_id,
            } => {
                self.pending_events.push_back(ToSwarm::GenerateEvent(
                    ClientEvent::ChunkPushReceived {
                        peer: overlay,
                        peer_id,
                        address,
                        data,
                        stamp,
                        request_id,
                    },
                ));
            }
            HandlerEvent::ReceiptReceived {
                overlay,
                address,
                signature,
                nonce,
                storage_radius,
            } => {
                self.pending_events.push_back(ToSwarm::GenerateEvent(
                    ClientEvent::ReceiptReceived {
                        peer: overlay,
                        address,
                        signature,
                        nonce,
                        storage_radius,
                    },
                ));
            }
            HandlerEvent::Error {
                overlay,
                protocol,
                error,
            } => {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(ClientEvent::ProtocolError {
                        peer: overlay,
                        peer_id: Some(peer_id),
                        protocol,
                        error,
                    }));
            }
        }
    }
}

impl NetworkBehaviour for SwarmClientBehaviour {
    type ConnectionHandler = SwarmClientHandler;
    type ToSwarm = ClientEvent;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // Create a dormant handler - will be activated after handshake
        Ok(SwarmClientHandler::new(self.config.handler.clone()))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: libp2p::core::transport::PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // Create a dormant handler - will be activated after handshake
        Ok(SwarmClientHandler::new(self.config.handler.clone()))
    }

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        match event {
            FromSwarm::ConnectionClosed(info) => {
                // Clean up peer tracking if last connection
                if info.remaining_established == 0 {
                    if let Some(overlay) = self.peer_overlays.remove(&info.peer_id) {
                        self.overlay_peers.remove(&overlay);
                        debug!(peer_id = %info.peer_id, %overlay, "Peer disconnected");
                        self.pending_events.push_back(ToSwarm::GenerateEvent(
                            ClientEvent::PeerDisconnected {
                                peer_id: info.peer_id,
                                overlay,
                            },
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        _connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.on_handler_event(peer_id, event);
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(event);
        }
        Poll::Pending
    }
}
