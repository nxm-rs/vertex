//! `ClientBehaviour`: the client-side protocols (pricing, retrieval, pushsync)
//! driven through a per-connection [`ClientHandler`]. Handlers are created
//! dormant and activated after handshake completion.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    task::{Context, Poll},
};

use alloy_primitives::U256;
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
use vertex_swarm_api::{Au, SwarmLocalStore};
use vertex_swarm_net_pseudosettle::PaymentAck;
use vertex_swarm_primitives::OverlayAddress;

#[cfg(feature = "swap")]
use vertex_swarm_client_protocol::SwapEvent;
use vertex_swarm_client_protocol::{
    ChunkTransferError, ClientCommand, ClientEvent, PseudosettleAck, PseudosettleEvent,
};

use super::{
    forward::Forwarder,
    handler::{ClientHandler, Config as HandlerConfig, HandlerCommand, HandlerEvent},
    storer::StorerCapability,
};

const DEFAULT_MAX_PENDING_EVENTS: usize = 4096;

#[derive(Debug, Clone)]
pub struct Config {
    pub handler: HandlerConfig,
    /// Pending-event queue cap; events past it are dropped.
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

impl Config {
    /// The handler's inbound protocol set is narrowed by role: bootnodes
    /// advertise pricing only, clients and storers the full set.
    pub fn for_role(local_role: vertex_swarm_primitives::SwarmNodeType) -> Self {
        let mut cfg = Self::default();
        cfg.handler.local_role = local_role;
        cfg
    }
}

/// Creates dormant handlers per connection and activates them on an
/// `ActivatePeer` command (sent after handshake completion). Settlement events
/// can additionally be routed to dedicated sinks via the `route_*` setters;
/// they are still emitted as [`ClientEvent`].
pub struct ClientBehaviour {
    config: Config,
    /// Cloned into each handler at connection establishment so inbound
    /// retrievals can serve from it.
    store: Arc<dyn SwarmLocalStore>,
    /// Cloned into each handler so a cache miss or pushsync can relay to a
    /// closer peer.
    forward: Arc<dyn Forwarder>,
    /// Present only on a storer. When set, deliveries the node is responsible
    /// for are stored and acknowledged with a signed receipt; when absent every
    /// inbound pushsync takes the verbatim-relay path.
    storer: Option<StorerCapability>,
    peer_overlays: HashMap<PeerId, OverlayAddress>,
    overlay_peers: HashMap<OverlayAddress, PeerId>,
    pending_events: VecDeque<ToSwarm<ClientEvent, HandlerCommand>>,
    pseudosettle_event_tx: Option<mpsc::UnboundedSender<PseudosettleEvent>>,
    #[cfg(feature = "swap")]
    swap_event_tx: Option<mpsc::UnboundedSender<SwapEvent>>,
}

impl ClientBehaviour {
    pub fn new(
        config: Config,
        store: Arc<dyn SwarmLocalStore>,
        forward: Arc<dyn Forwarder>,
    ) -> Self {
        Self {
            config,
            store,
            forward,
            storer: None,
            peer_overlays: HashMap::new(),
            overlay_peers: HashMap::new(),
            pending_events: VecDeque::new(),
            pseudosettle_event_tx: None,
            #[cfg(feature = "swap")]
            swap_event_tx: None,
        }
    }

    /// Install the storer ingest capability, turning inbound pushsync into a
    /// store-and-sign path for chunks this node is responsible for. Only a
    /// storer installs this; a client keeps the verbatim-relay path.
    ///
    /// Must run before any peer connects: handlers clone it at connection setup.
    pub fn set_storer(&mut self, storer: StorerCapability) {
        self.storer = Some(storer);
    }

    /// Install the multi-hop relay forwarder, replacing the default stub.
    ///
    /// Must run before any peer connects: handlers clone it at connection setup.
    pub fn set_forwarder(&mut self, forward: Arc<dyn Forwarder>) {
        self.forward = forward;
    }

    /// Network id used to recover an inbound custody receipt's signer at the
    /// decode boundary.
    ///
    /// Must run before any peer connects: handlers clone the config at connection
    /// setup.
    pub fn set_network_id(&mut self, network_id: nectar_primitives::NetworkId) {
        self.config.handler.network_id = network_id;
    }

    fn new_handler(&self) -> ClientHandler {
        ClientHandler::new(
            self.config.handler.clone(),
            Arc::clone(&self.store),
            Arc::clone(&self.forward),
            self.storer.clone(),
        )
    }

    /// Also send pseudosettle events to `tx` (still emitted as [`ClientEvent`]).
    pub fn route_pseudosettle_events(&mut self, tx: mpsc::UnboundedSender<PseudosettleEvent>) {
        self.pseudosettle_event_tx = Some(tx);
    }

    /// Also send swap events to `tx` (still emitted as [`ClientEvent`]).
    #[cfg(feature = "swap")]
    // Wired by the node builder when the swap settlement service is present.
    #[allow(dead_code)]
    pub fn route_swap_events(&mut self, tx: mpsc::UnboundedSender<SwapEvent>) {
        self.swap_event_tx = Some(tx);
    }

    fn push_event(&mut self, event: ToSwarm<ClientEvent, HandlerCommand>) {
        if self.pending_events.len() >= self.config.max_pending_events {
            warn!("Behaviour event queue full, dropping event");
            metrics::counter!("swarm.client.behaviour.events_dropped").increment(1);
            return;
        }
        self.pending_events.push_back(event);
    }

    pub fn on_command(&mut self, command: ClientCommand) {
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
                originated,
            } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %address, "Retrieving chunk");
                    self.push_event(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::RetrieveChunk {
                            address,
                            response,
                            originated,
                        },
                    });
                } else {
                    debug!(%peer, "Unknown peer for retrieval");
                    let _ = response.send(Err(ChunkTransferError::NotConnected));
                }
            }
            ClientCommand::PushChunk {
                peer,
                address,
                chunk,
                response,
                originated,
            } => {
                if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    debug!(%peer_id, %peer, %address, "Pushing chunk");
                    self.push_event(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: libp2p::swarm::NotifyHandler::Any,
                        event: HandlerCommand::PushChunk {
                            chunk,
                            response,
                            originated,
                        },
                    });
                } else {
                    debug!(%peer, "Unknown peer for push");
                    let _ = response.send(Err(ChunkTransferError::NotConnected));
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
                        event: HandlerCommand::AckPseudosettle {
                            request_id,
                            ack: wire_ack(ack),
                        },
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
            // `ClientCommand` carries swap variants when `client-protocol/swap`
            // is on, which Cargo feature unification can turn on (a workspace
            // build also compiling `accounting-swap`) even when this crate's
            // `swap` feature is off. The swap wire is then not linked here, so
            // drop the command. The all-features build keeps full exhaustiveness.
            // Unreachable when nothing in the build enables `client-protocol/swap`.
            #[cfg(not(feature = "swap"))]
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }

    fn on_handler_event(&mut self, peer_id: PeerId, event: HandlerEvent) {
        match event {
            HandlerEvent::Activated { overlay } => {
                debug!(%peer_id, %overlay, "Handler activated");
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
            HandlerEvent::InboundServed { overlay } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::InboundServed {
                    peer: overlay,
                }));
            }
            HandlerEvent::InboundForwarded { overlay } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::InboundForwarded {
                    peer: overlay,
                }));
            }
            HandlerEvent::InboundMissed { overlay, address } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::InboundMissed {
                    peer: overlay,
                    address,
                }));
            }
            HandlerEvent::InboundRelayed { overlay } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::InboundRelayed {
                    peer: overlay,
                }));
            }
            HandlerEvent::InboundStored { overlay } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::InboundStored {
                    peer: overlay,
                }));
            }
            HandlerEvent::InboundPushFailed { overlay, address } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::InboundPushFailed {
                    peer: overlay,
                    address,
                }));
            }
            HandlerEvent::ChunkReceived {
                overlay,
                address,
                chunk,
                stamp,
                latency,
                originated,
            } => {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(ClientEvent::ChunkReceived {
                        peer: overlay,
                        address,
                        chunk,
                        stamp,
                        latency,
                        originated,
                    }));
            }
            HandlerEvent::ReceiptReceived {
                overlay,
                address,
                latency,
                originated,
            } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::ReceiptReceived {
                    peer: overlay,
                    address,
                    latency,
                    originated,
                }));
            }
            HandlerEvent::RetrievalFailed {
                overlay,
                address,
                error,
                kind,
            } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::RetrievalFailed {
                    peer: overlay,
                    address,
                    error,
                    kind,
                }));
            }
            HandlerEvent::PushFailed {
                overlay,
                address,
                error,
                kind,
            } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::PushFailed {
                    peer: overlay,
                    address,
                    error,
                    kind,
                }));
            }
            HandlerEvent::InboundInvalidData { overlay, protocol } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::InboundInvalidData {
                    peer: overlay,
                    protocol,
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
                let ack = domain_ack(ack);
                if let Some(tx) = &self.pseudosettle_event_tx
                    && tx
                        .send(PseudosettleEvent::Sent { peer: overlay, ack })
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
        Ok(self.new_handler())
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: libp2p::core::transport::PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(self.new_handler())
    }

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        if let FromSwarm::ConnectionClosed(info) = event
            && info.remaining_established == 0
            && let Some(overlay) = self.peer_overlays.remove(&info.peer_id)
        {
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

/// Assemble the wire ack from the deciding service's domain decision.
///
/// The clock was sampled in the deciding service and is preserved verbatim; only
/// the amount crosses the AU boundary here.
fn wire_ack(ack: PseudosettleAck) -> PaymentAck {
    PaymentAck::new(U256::from(ack.accepted.as_amount()), ack.timestamp)
}

/// Convert a decoded wire ack into the domain decision.
///
/// In-spec pseudosettle amounts fit in a `u64` of AU; a larger wire value is out
/// of spec and saturates to the maximum AU so the deciding service still detects
/// the over-acceptance in AU space rather than wrapping to a small amount. The
/// responder's sampled timestamp passes through unchanged.
fn domain_ack(ack: PaymentAck) -> PseudosettleAck {
    let accepted = if ack.amount > U256::from(u64::MAX) {
        Au::from_amount(u64::MAX)
    } else {
        Au::from_amount(ack.amount.as_limbs()[0])
    };
    PseudosettleAck {
        accepted,
        timestamp: ack.timestamp,
    }
}
