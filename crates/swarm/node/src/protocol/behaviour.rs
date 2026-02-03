//! ClientBehaviour for managing client-side protocols.
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
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};
use vertex_swarm_primitives::OverlayAddress;

use super::{
    ClientCommand, ClientEvent, PseudosettleEvent, SwapEvent,
    handler::{ClientHandler, Config as HandlerConfig, HandlerCommand, HandlerEvent},
};

/// Configuration for the client behaviour.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Handler configuration.
    pub handler: HandlerConfig,
}

/// The ClientBehaviour manages client-side protocols.
///
/// It creates handlers in dormant state for each connection, and activates
/// them after receiving `ActivatePeer` commands (typically sent after
/// handshake completion).
///
/// # Event Routing
///
/// Settlement events can be routed directly to settlement services via optional
/// event senders. Use [`set_pseudosettle_events`](Self::set_pseudosettle_events)
/// and [`set_swap_events`](Self::set_swap_events) to configure routing.
/// Events are still emitted as [`ClientEvent`] for other consumers.
pub struct ClientBehaviour {
    config: Config,
    /// Map of peer_id -> overlay for activated peers.
    peer_overlays: HashMap<PeerId, OverlayAddress>,
    /// Map of overlay -> peer_id for reverse lookup.
    overlay_peers: HashMap<OverlayAddress, PeerId>,
    /// Pending events to emit.
    pending_events: VecDeque<ToSwarm<ClientEvent, HandlerCommand>>,
    /// Optional sender for pseudosettle events.
    pseudosettle_event_tx: Option<mpsc::UnboundedSender<PseudosettleEvent>>,
    /// Optional sender for swap events.
    swap_event_tx: Option<mpsc::UnboundedSender<SwapEvent>>,
}

impl ClientBehaviour {
    /// Create a new client behaviour with the given configuration.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            peer_overlays: HashMap::new(),
            overlay_peers: HashMap::new(),
            pending_events: VecDeque::new(),
            pseudosettle_event_tx: None,
            swap_event_tx: None,
        }
    }

    /// Set the sender for pseudosettle events.
    ///
    /// When set, pseudosettle-related events will be sent to this channel
    /// in addition to being emitted as [`ClientEvent`].
    pub fn set_pseudosettle_events(&mut self, tx: mpsc::UnboundedSender<PseudosettleEvent>) {
        self.pseudosettle_event_tx = Some(tx);
    }

    /// Set the sender for swap events.
    ///
    /// When set, swap-related events will be sent to this channel
    /// in addition to being emitted as [`ClientEvent`].
    pub fn set_swap_events(&mut self, tx: mpsc::UnboundedSender<SwapEvent>) {
        self.swap_event_tx = Some(tx);
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
                if self.overlay_peers.contains_key(&peer) {
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
                if self.overlay_peers.contains_key(&peer) {
                    trace!("SendReceipt not yet implemented");
                }
            }
            ClientCommand::SendCheque {
                peer,
                cheque,
                our_rate,
            } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, "Sending swap cheque");
                    self.pending_events.push_back(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::SendCheque { cheque, our_rate },
                    });
                } else {
                    debug!(%peer, "Unknown peer for cheque");
                }
            }
            ClientCommand::SendPseudosettle { peer, amount } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %amount, "Sending pseudosettle");
                    self.pending_events.push_back(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::SendPseudosettle { amount },
                    });
                } else {
                    debug!(%peer, "Unknown peer for pseudosettle");
                }
            }
            ClientCommand::AckPseudosettle {
                peer,
                request_id,
                ack,
            } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %request_id, "Acking pseudosettle");
                    self.pending_events.push_back(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::AckPseudosettle { request_id, ack },
                    });
                } else {
                    debug!(%peer, "Unknown peer for pseudosettle ack");
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
            HandlerEvent::PseudosettleReceived {
                overlay,
                amount,
                request_id,
            } => {
                // Route to pseudosettle service if configured
                if let Some(tx) = &self.pseudosettle_event_tx
                    && tx
                        .send(PseudosettleEvent::Received {
                            peer: overlay,
                            amount,
                            request_id,
                        })
                        .is_err()
                {
                    warn!(%overlay, "Pseudosettle event channel closed");
                }
                self.pending_events.push_back(ToSwarm::GenerateEvent(
                    ClientEvent::PseudosettleReceived {
                        peer: overlay,
                        peer_id,
                        amount,
                        request_id,
                    },
                ));
            }
            HandlerEvent::PseudosettleSent { overlay, ack } => {
                // Route to pseudosettle service if configured
                if let Some(tx) = &self.pseudosettle_event_tx
                    && tx
                        .send(PseudosettleEvent::Sent {
                            peer: overlay,
                            ack: ack.clone(),
                        })
                        .is_err()
                {
                    warn!(%overlay, "Pseudosettle event channel closed");
                }
                self.pending_events.push_back(ToSwarm::GenerateEvent(
                    ClientEvent::PseudosettleSent {
                        peer: overlay,
                        peer_id,
                        ack,
                    },
                ));
            }
            HandlerEvent::ChequeReceived {
                overlay,
                cheque,
                peer_rate,
            } => {
                // Route to swap service if configured
                if let Some(tx) = &self.swap_event_tx
                    && tx
                        .send(SwapEvent::ChequeReceived {
                            peer: overlay,
                            cheque: cheque.clone(),
                            peer_rate,
                        })
                        .is_err()
                {
                    warn!(%overlay, "Swap event channel closed");
                }
                self.pending_events.push_back(ToSwarm::GenerateEvent(
                    ClientEvent::ChequeReceived {
                        peer: overlay,
                        peer_id,
                        cheque,
                        peer_rate,
                    },
                ));
            }
            HandlerEvent::ChequeSent { overlay, peer_rate } => {
                // Route to swap service if configured
                if let Some(tx) = &self.swap_event_tx
                    && tx
                        .send(SwapEvent::ChequeSent {
                            peer: overlay,
                            peer_rate,
                        })
                        .is_err()
                {
                    warn!(%overlay, "Swap event channel closed");
                }
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(ClientEvent::ChequeSent {
                        peer: overlay,
                        peer_id,
                        peer_rate,
                    }));
            }
        }
    }
}

impl NetworkBehaviour for ClientBehaviour {
    type ConnectionHandler = ClientHandler;
    type ToSwarm = ClientEvent;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // Create a dormant handler - will be activated after handshake
        Ok(ClientHandler::new(self.config.handler.clone()))
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
        Ok(ClientHandler::new(self.config.handler.clone()))
    }

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        if let FromSwarm::ConnectionClosed(info) = event
            && info.remaining_established == 0
            && let Some(overlay) = self.peer_overlays.remove(&info.peer_id)
        {
            // Clean up peer tracking if last connection
            self.overlay_peers.remove(&overlay);
            debug!(peer_id = %info.peer_id, %overlay, "Peer disconnected");
            self.pending_events
                .push_back(ToSwarm::GenerateEvent(ClientEvent::PeerDisconnected {
                    peer_id: info.peer_id,
                    overlay,
                }));
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
