//! ClientBehaviour for managing client-side protocols.
//!
//! This behaviour manages multiple client protocols (credit, retrieval, pushsync)
//! using a per-connection handler pattern. Handlers start in dormant state and are
//! activated after handshake completion.

use std::{
    sync::Arc,
    task::{Context, Poll},
};

use libp2p::{
    Multiaddr, PeerId,
    core::Endpoint,
    swarm::{
        ConnectionDenied, ConnectionId, NetworkBehaviour, THandler, THandlerInEvent,
        THandlerOutEvent, ToSwarm,
    },
};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::{
    ClientCommand, ClientEvent, PeerAddressResolver, PseudosettleEvent,
    handler::{ClientHandler, Config as HandlerConfig, HandlerCommand, HandlerEvent},
    queue::BoundedEventQueue,
};

const DEFAULT_MAX_PENDING_EVENTS: usize = 4096;

/// Configuration for the client behaviour.
#[derive(Debug, Clone)]
pub struct Config {
    /// Handler configuration.
    pub handler: HandlerConfig,
    /// Maximum pending events before dropping new ones.
    pub max_pending_events: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            handler: HandlerConfig::default(),
            max_pending_events: DEFAULT_MAX_PENDING_EVENTS,
        }
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
/// event senders. Use [`set_pseudosettle_events`](Self::set_pseudosettle_events)
/// to configure routing. Events are still emitted as [`ClientEvent`] for other consumers.
pub struct ClientBehaviour {
    config: Config,
    /// Resolver for looking up peer IDs from overlay addresses.
    resolver: Arc<dyn PeerAddressResolver>,
    /// Pending events to emit.
    pending_events: BoundedEventQueue<ToSwarm<ClientEvent, HandlerCommand>>,
    /// Optional sender for pseudosettle events.
    pseudosettle_event_tx: Option<mpsc::UnboundedSender<PseudosettleEvent>>,
}

impl ClientBehaviour {
    /// Create a new client behaviour with the given configuration and resolver.
    pub fn new(config: Config, resolver: Arc<dyn PeerAddressResolver>) -> Self {
        let pending_events = BoundedEventQueue::new(
            config.max_pending_events,
            "swarm.client.behaviour.events_dropped",
        );
        Self {
            config,
            resolver,
            pending_events,
            pseudosettle_event_tx: None,
        }
    }

    /// Set the sender for pseudosettle events.
    ///
    /// When set, pseudosettle-related events will be sent to this channel
    /// in addition to being emitted as [`ClientEvent`].
    pub fn set_pseudosettle_events(&mut self, tx: mpsc::UnboundedSender<PseudosettleEvent>) {
        self.pseudosettle_event_tx = Some(tx);
    }

    /// Handle a command from the application layer.
    pub fn on_command(&mut self, command: ClientCommand) {
        match command {
            ClientCommand::ActivatePeer {
                peer_id,
                overlay,
                node_type,
            } => {
                debug!(%peer_id, %overlay, ?node_type, "Activating peer");
                self.pending_events.push(ToSwarm::NotifyHandler {
                    peer_id,
                    handler: libp2p::swarm::NotifyHandler::Any,
                    event: HandlerCommand::Activate { overlay, node_type },
                });
            }
            ClientCommand::AnnounceCreditLimit { peer, credit_limit } => {
                if let Some(peer_id) = self.resolver.peer_id_for_overlay(&peer) {
                    debug!(%peer_id, %peer, %credit_limit, "Announcing credit limit");
                    self.pending_events.push(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::AnnounceCreditLimit { credit_limit },
                    });
                } else {
                    debug!(%peer, "Unknown peer for credit limit announcement");
                }
            }
            ClientCommand::RetrieveChunk { peer, address } => {
                if let Some(peer_id) = self.resolver.peer_id_for_overlay(&peer) {
                    debug!(%peer_id, %peer, %address, "Retrieving chunk");
                    self.pending_events.push(ToSwarm::NotifyHandler {
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
                if let Some(peer_id) = self.resolver.peer_id_for_overlay(&peer) {
                    debug!(%peer_id, %peer, %address, "Pushing chunk");
                    self.pending_events.push(ToSwarm::NotifyHandler {
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
                request_id,
                data,
                stamp,
            } => {
                if let Some(peer_id) = self.resolver.peer_id_for_overlay(&peer) {
                    debug!(%peer_id, %peer, %request_id, "Serving chunk");
                    self.pending_events.push(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::ServeChunk {
                            request_id,
                            data,
                            stamp,
                        },
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
                if let Some(peer_id) = self.resolver.peer_id_for_overlay(&peer) {
                    debug!(%peer_id, %peer, %request_id, "Sending receipt");
                    self.pending_events.push(ToSwarm::NotifyHandler {
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
                if let Some(peer_id) = self.resolver.peer_id_for_overlay(&peer) {
                    debug!(%peer_id, %peer, %amount, "Sending pseudosettle");
                    self.pending_events.push(ToSwarm::NotifyHandler {
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
                if let Some(peer_id) = self.resolver.peer_id_for_overlay(&peer) {
                    debug!(%peer_id, %peer, %request_id, "Acking pseudosettle");
                    self.pending_events.push(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::AckPseudosettle { request_id, ack },
                    });
                } else {
                    debug!(%peer, "Unknown peer for pseudosettle ack");
                }
            }
            ClientCommand::DisconnectPeer { peer, reason } => {
                if let Some(peer_id) = self.resolver.peer_id_for_overlay(&peer) {
                    debug!(%peer_id, %peer, ?reason, "Disconnecting peer");
                    self.pending_events.push(ToSwarm::CloseConnection {
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
                    .push_unchecked(ToSwarm::GenerateEvent(ClientEvent::PeerActivated {
                        peer_id,
                        overlay,
                    }));
            }
            HandlerEvent::CreditLimitReceived {
                overlay,
                credit_limit,
            } => {
                self.pending_events.push(ToSwarm::GenerateEvent(ClientEvent::CreditLimitReceived {
                    peer: overlay,
                    peer_id,
                    credit_limit,
                }));
            }
            HandlerEvent::CreditLimitSent { overlay } => {
                self.pending_events.push_unchecked(ToSwarm::GenerateEvent(
                    ClientEvent::CreditLimitSent { peer: overlay },
                ));
            }
            HandlerEvent::ChunkRequested {
                overlay,
                address,
                request_id,
            } => {
                self.pending_events.push(ToSwarm::GenerateEvent(ClientEvent::ChunkRequested {
                    peer: overlay,
                    peer_id,
                    address,
                    request_id,
                }));
            }
            HandlerEvent::ChunkReceived {
                overlay,
                address,
                data,
                stamp,
            } => {
                self.pending_events
                    .push_unchecked(ToSwarm::GenerateEvent(ClientEvent::ChunkReceived {
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
                self.pending_events.push(ToSwarm::GenerateEvent(ClientEvent::ChunkPushReceived {
                    peer: overlay,
                    peer_id,
                    address,
                    data,
                    stamp,
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
                self.pending_events.push(ToSwarm::GenerateEvent(ClientEvent::ReceiptReceived {
                    peer: overlay,
                    address,
                    signature,
                    nonce,
                    storage_radius,
                }));
            }
            HandlerEvent::Error {
                overlay,
                protocol,
                error,
            } => {
                self.pending_events
                    .push_unchecked(ToSwarm::GenerateEvent(ClientEvent::ProtocolError {
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
                self.pending_events.push(ToSwarm::GenerateEvent(ClientEvent::PseudosettleReceived {
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
                self.pending_events.push(ToSwarm::GenerateEvent(ClientEvent::PseudosettleSent {
                    peer: overlay,
                    peer_id,
                    ack,
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

    fn on_swarm_event(&mut self, _event: libp2p::swarm::FromSwarm<'_>) {
        // Disconnect handling moved to the node event loop (via TopologyEvent::PeerDisconnected).
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
        if let Some(event) = self.pending_events.pop() {
            return Poll::Ready(event);
        }
        Poll::Pending
    }
}
