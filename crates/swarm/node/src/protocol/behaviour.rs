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
use tracing::{debug, warn};
use vertex_swarm_primitives::OverlayAddress;

#[cfg(feature = "swap")]
use super::SwapEvent;
use super::{
    ClientCommand, ClientEvent, PseudosettleEvent,
    handler::{ClientHandler, Config as HandlerConfig, HandlerCommand, HandlerEvent},
};

const DEFAULT_MAX_PENDING_EVENTS: usize = 4096;

/// Configuration for the client behaviour.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Handler configuration.
    pub(crate) handler: HandlerConfig,
    /// Maximum pending events before dropping new ones.
    pub(crate) max_pending_events: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            handler: HandlerConfig::default(),
            max_pending_events: DEFAULT_MAX_PENDING_EVENTS,
        }
    }
}

impl Config {
    /// Build a config for the given local node role. The handler's inbound
    /// protocol set is narrowed by role: bootnodes advertise pricing only,
    /// clients and storers advertise the full client protocol set.
    pub(crate) fn for_role(local_role: vertex_swarm_primitives::SwarmNodeType) -> Self {
        let mut cfg = Self::default();
        cfg.handler.local_role = local_role;
        cfg
    }
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
/// event senders. Use [`route_pseudosettle_events`](Self::route_pseudosettle_events)
/// to configure routing. Events are still emitted as [`ClientEvent`] for other consumers.
pub(crate) struct ClientBehaviour {
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
    #[cfg(feature = "swap")]
    swap_event_tx: Option<mpsc::UnboundedSender<SwapEvent>>,
}

impl ClientBehaviour {
    /// Create a new client behaviour with the given configuration.
    pub(crate) fn new(config: Config) -> Self {
        Self {
            config,
            peer_overlays: HashMap::new(),
            overlay_peers: HashMap::new(),
            pending_events: VecDeque::new(),
            pseudosettle_event_tx: None,
            #[cfg(feature = "swap")]
            swap_event_tx: None,
        }
    }

    /// Route pseudosettle events to the given sink.
    ///
    /// Once routed, pseudosettle-related events will be sent to this channel
    /// in addition to being emitted as [`ClientEvent`].
    pub(crate) fn route_pseudosettle_events(
        &mut self,
        tx: mpsc::UnboundedSender<PseudosettleEvent>,
    ) {
        self.pseudosettle_event_tx = Some(tx);
    }

    /// Route swap events to the given sink.
    ///
    /// Once routed, swap-related events will be sent to this channel in addition
    /// to being emitted as [`ClientEvent`]. The node wires this up when a swap
    /// settlement service is present.
    #[cfg(feature = "swap")]
    // Wired by the node builder when the swap settlement service is present.
    #[allow(dead_code)]
    pub(crate) fn route_swap_events(&mut self, tx: mpsc::UnboundedSender<SwapEvent>) {
        self.swap_event_tx = Some(tx);
    }

    /// Push an event if the queue isn't full, otherwise drop with a metric.
    fn push_event(&mut self, event: ToSwarm<ClientEvent, HandlerCommand>) {
        if self.pending_events.len() >= self.config.max_pending_events {
            warn!("Behaviour event queue full, dropping event");
            metrics::counter!("swarm.client.behaviour.events_dropped").increment(1);
            return;
        }
        self.pending_events.push_back(event);
    }

    /// Handle a command from the application layer.
    pub(crate) fn on_command(&mut self, command: ClientCommand) {
        match command {
            ClientCommand::ActivatePeer {
                peer_id,
                overlay,
                node_type,
            } => {
                debug!(%peer_id, %overlay, ?node_type, "Activating peer");
                self.peer_overlays.insert(peer_id, overlay);
                self.overlay_peers.insert(overlay, peer_id);
                self.push_event(ToSwarm::NotifyHandler {
                    peer_id,
                    handler: libp2p::swarm::NotifyHandler::Any,
                    event: HandlerCommand::Activate { overlay, node_type },
                });
            }
            ClientCommand::AnnouncePricing { peer, threshold } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %threshold, "Announcing pricing");
                    self.push_event(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::AnnouncePricing { threshold },
                    });
                } else {
                    debug!(%peer, "Unknown peer for pricing announcement");
                }
            }
            ClientCommand::RetrieveChunk {
                peer,
                address,
                response,
            } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %address, "Retrieving chunk");
                    self.push_event(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::RetrieveChunk { address, response },
                    });
                } else {
                    debug!(%peer, "Unknown peer for retrieval");
                    let _ = response.send(Err(crate::RetrievalError::NotConnected));
                }
            }
            ClientCommand::PushChunk {
                peer,
                address,
                chunk,
                response,
            } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %address, "Pushing chunk");
                    self.push_event(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::PushChunk { chunk, response },
                    });
                } else {
                    debug!(%peer, "Unknown peer for push");
                    let _ = response.send(Err(crate::RetrievalError::NotConnected));
                }
            }
            ClientCommand::ServeChunk {
                peer,
                request_id,
                address: _,
                chunk,
            } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %request_id, "Serving chunk");
                    self.push_event(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::ServeChunk { request_id, chunk },
                    });
                } else {
                    debug!(%peer, "Unknown peer for serve chunk");
                }
            }
            ClientCommand::SendReceipt {
                peer,
                request_id,
                address,
                signature,
                nonce,
                storage_radius,
            } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %request_id, "Sending receipt");
                    self.push_event(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::SendReceipt {
                            request_id,
                            address,
                            signature,
                            nonce,
                            storage_radius,
                        },
                    });
                } else {
                    debug!(%peer, "Unknown peer for send receipt");
                }
            }
            ClientCommand::SendPseudosettle { peer, amount } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %amount, "Sending pseudosettle");
                    self.push_event(ToSwarm::NotifyHandler {
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
                    self.push_event(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::AckPseudosettle { request_id, ack },
                    });
                } else {
                    debug!(%peer, "Unknown peer for pseudosettle ack");
                }
            }
            #[cfg(feature = "swap")]
            ClientCommand::SendCheque { peer, cheque } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, "Sending swap cheque");
                    self.push_event(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::SendCheque { cheque },
                    });
                } else {
                    debug!(%peer, "Unknown peer for swap cheque");
                }
            }
            ClientCommand::DisconnectPeer { peer, reason } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, ?reason, "Disconnecting peer");
                    self.push_event(ToSwarm::CloseConnection {
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
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::PricingReceived {
                    peer: overlay,
                    peer_id,
                    threshold,
                }));
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
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::ChunkRequested {
                    peer: overlay,
                    peer_id,
                    address,
                    request_id,
                }));
            }
            HandlerEvent::ChunkReceived {
                overlay,
                address,
                chunk,
            } => {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(ClientEvent::ChunkReceived {
                        peer: overlay,
                        address,
                        chunk,
                    }));
            }
            HandlerEvent::ChunkPushReceived {
                overlay,
                address,
                chunk,
                request_id,
            } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::ChunkPushReceived {
                    peer: overlay,
                    peer_id,
                    address,
                    chunk,
                    request_id,
                }));
            }
            HandlerEvent::ReceiptReceived {
                overlay,
                address,
                signature,
                nonce,
                storage_radius,
            } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::ReceiptReceived {
                    peer: overlay,
                    address,
                    signature,
                    nonce,
                    storage_radius,
                }));
            }
            HandlerEvent::RetrievalFailed {
                overlay,
                address,
                error,
            } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::RetrievalFailed {
                    peer: overlay,
                    address,
                    error,
                }));
            }
            HandlerEvent::PushFailed {
                overlay,
                address,
                error,
            } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::PushFailed {
                    peer: overlay,
                    address,
                    error,
                }));
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
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::PseudosettleReceived {
                    peer: overlay,
                    peer_id,
                    amount,
                    request_id,
                }));
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
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::PseudosettleSent {
                    peer: overlay,
                    peer_id,
                    ack,
                }));
            }
            #[cfg(feature = "swap")]
            HandlerEvent::SwapChequeReceived {
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
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::SwapChequeReceived {
                    peer: overlay,
                    peer_id,
                    cheque,
                    peer_rate,
                }));
            }
            #[cfg(feature = "swap")]
            HandlerEvent::SwapChequeSent { overlay, peer_rate } => {
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
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::SwapChequeSent {
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
