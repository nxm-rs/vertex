//! ClientBehaviour for managing client-side protocols.
//!
//! This behaviour manages multiple client protocols (pricing, retrieval, pushsync)
//! using a per-connection handler pattern. Handlers start in dormant state and are
//! activated after handshake completion.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
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
use vertex_swarm_api::SwarmLocalStore;
use vertex_swarm_primitives::OverlayAddress;

#[cfg(feature = "swap")]
use super::SwapEvent;
use super::{
    ClientCommand, ClientEvent, PseudosettleEvent,
    forward::Forwarder,
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
    /// The client cache, cloned into each handler at connection establishment so
    /// inbound retrievals can serve from it.
    store: Arc<dyn SwarmLocalStore>,
    /// The forwarder seam, cloned into each handler so a cache miss or a
    /// pushsync can relay to a closer peer.
    forward: Arc<dyn Forwarder>,
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
    /// Create a new client behaviour with the given configuration, cache, and
    /// forwarder.
    pub(crate) fn new(
        config: Config,
        store: Arc<dyn SwarmLocalStore>,
        forward: Arc<dyn Forwarder>,
    ) -> Self {
        Self {
            config,
            store,
            forward,
            peer_overlays: HashMap::new(),
            overlay_peers: HashMap::new(),
            pending_events: VecDeque::new(),
            pseudosettle_event_tx: None,
            #[cfg(feature = "swap")]
            swap_event_tx: None,
        }
    }

    /// Build a dormant handler for a new connection, sharing the cache and
    /// forwarder into it.
    fn new_handler(&self) -> ClientHandler {
        ClientHandler::new(
            self.config.handler.clone(),
            Arc::clone(&self.store),
            Arc::clone(&self.forward),
        )
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
                    let _ = response.send(Err(crate::ChunkTransferError::NotConnected));
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
                    let _ = response.send(Err(crate::ChunkTransferError::NotConnected));
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
                latency,
            } => {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(ClientEvent::ChunkReceived {
                        peer: overlay,
                        address,
                        chunk,
                        latency,
                    }));
            }
            HandlerEvent::ReceiptReceived {
                overlay,
                address,
                latency,
            } => {
                self.push_event(ToSwarm::GenerateEvent(ClientEvent::ReceiptReceived {
                    peer: overlay,
                    address,
                    latency,
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
        // Create a dormant handler - will be activated after handshake
        Ok(self.new_handler())
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use alloy_primitives::{B256, Signature};
    use alloy_signer_local::PrivateKeySigner;
    use futures::StreamExt;
    use libp2p::Swarm;
    use libp2p_swarm_test::SwarmExt;
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk, SingleOwnerChunk};
    use tokio::sync::oneshot;
    use vertex_swarm_api::SwarmLocalStore;
    use vertex_swarm_localstore::{ChunkStore, Clock};
    use vertex_swarm_primitives::{OverlayAddress, StampedChunk, SwarmNodeType};

    use super::{ClientBehaviour, ClientCommand, Config};
    use crate::client_service::RetrievalResult;
    use crate::protocol::StubForwarder;

    /// A clock fixed at a single instant, for SOC freshness tests.
    struct FixedClock(i64);

    impl Clock for FixedClock {
        fn now_ns(&self) -> i64 {
            self.0
        }
    }

    fn content_chunk(payload: &'static [u8]) -> StampedChunk {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
        let chunk: AnyChunk = ContentChunk::new(payload)
            .expect("valid content chunk")
            .into();
        StampedChunk::new(chunk, stamp)
    }

    fn soc_chunk(payload: &'static [u8], stamp_ns: u64) -> StampedChunk {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, stamp_ns, sig);
        let signer = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x11)).expect("signer");
        let chunk: AnyChunk = SingleOwnerChunk::new(B256::repeat_byte(0x22), payload, &signer)
            .expect("valid soc")
            .into();
        StampedChunk::new(chunk, stamp)
    }

    fn overlay(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    /// Build a swarm whose `ClientBehaviour` serves from `store`.
    fn swarm_with_store(store: Arc<dyn SwarmLocalStore>) -> Swarm<ClientBehaviour> {
        Swarm::new_ephemeral_tokio(move |_| {
            ClientBehaviour::new(
                Config::for_role(SwarmNodeType::Client),
                store,
                Arc::new(StubForwarder),
            )
        })
    }

    /// Connect two client swarms and activate both handlers with the given
    /// overlays so the request/serve path is live.
    async fn connect_and_activate(
        client: &mut Swarm<ClientBehaviour>,
        server: &mut Swarm<ClientBehaviour>,
        client_overlay: OverlayAddress,
        server_overlay: OverlayAddress,
    ) {
        let client_peer = *client.local_peer_id();
        let server_peer = *server.local_peer_id();
        client.listen().with_memory_addr_external().await;
        server.listen().with_memory_addr_external().await;
        client.connect(server).await;

        // Activate each side's handler for the other peer.
        client
            .behaviour_mut()
            .on_command(ClientCommand::ActivatePeer {
                peer_id: server_peer,
                overlay: server_overlay,
                node_type: SwarmNodeType::Client,
            });
        server
            .behaviour_mut()
            .on_command(ClientCommand::ActivatePeer {
                peer_id: client_peer,
                overlay: client_overlay,
                node_type: SwarmNodeType::Client,
            });
    }

    /// Drive both swarms until the retrieval response channel resolves.
    async fn drive_until_retrieved(
        client: &mut Swarm<ClientBehaviour>,
        server: &mut Swarm<ClientBehaviour>,
        mut rx: oneshot::Receiver<Result<RetrievalResult, crate::ChunkTransferError>>,
    ) -> Result<RetrievalResult, crate::ChunkTransferError> {
        let drive = async {
            loop {
                tokio::select! {
                    _ = client.select_next_some() => {}
                    _ = server.select_next_some() => {}
                    res = &mut rx => return res.expect("sender not dropped"),
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), drive)
            .await
            .expect("retrieval resolved within timeout")
    }

    #[tokio::test]
    async fn serves_a_content_chunk_from_the_cache() {
        let chunk = content_chunk(b"served from cache");
        let address = *chunk.address();

        let server_store: Arc<dyn SwarmLocalStore> =
            Arc::new(ChunkStore::with_budget(1 << 20, 1_000_000_000));
        server_store.put(chunk.clone()).unwrap();

        let mut client = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
        let mut server = swarm_with_store(server_store);

        let server_overlay = overlay(2);
        connect_and_activate(&mut client, &mut server, overlay(1), server_overlay).await;

        let (tx, rx) = oneshot::channel();
        client
            .behaviour_mut()
            .on_command(ClientCommand::RetrieveChunk {
                peer: server_overlay,
                address,
                response: tx,
            });

        let result = drive_until_retrieved(&mut client, &mut server, rx).await;
        let delivered = result.expect("served from cache");
        assert_eq!(*delivered.chunk.address(), address);
        assert_eq!(delivered.chunk, chunk);
    }

    #[tokio::test]
    async fn serves_a_fresh_soc_from_the_cache() {
        // A single-owner chunk stamped at 900ns, served at 1000ns under a 500ns
        // TTL: still fresh, so it serves from cache.
        let chunk = soc_chunk(b"feed v1", 900);
        let address = *chunk.address();

        let server_store: Arc<dyn SwarmLocalStore> = Arc::new(ChunkStore::with_budget_and_clock(
            1 << 20,
            500,
            FixedClock(1_000),
        ));
        server_store.put(chunk.clone()).unwrap();

        let mut client = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
        let mut server = swarm_with_store(server_store);

        let server_overlay = overlay(2);
        connect_and_activate(&mut client, &mut server, overlay(1), server_overlay).await;

        let (tx, rx) = oneshot::channel();
        client
            .behaviour_mut()
            .on_command(ClientCommand::RetrieveChunk {
                peer: server_overlay,
                address,
                response: tx,
            });

        let delivered = drive_until_retrieved(&mut client, &mut server, rx)
            .await
            .expect("fresh SOC served from cache");
        assert_eq!(delivered.chunk, chunk);
    }

    #[tokio::test]
    async fn expired_soc_is_not_served_and_resets() {
        // The same SOC stamped at 900ns but served at 2000ns under a 500ns TTL is
        // expired: the cache reads it as a miss and, with the stub forwarder, the
        // inbound retrieval resets instead of serving a stale revision.
        let chunk = soc_chunk(b"feed v1", 900);
        let address = *chunk.address();

        let server_store: Arc<dyn SwarmLocalStore> = Arc::new(ChunkStore::with_budget_and_clock(
            1 << 20,
            500,
            FixedClock(2_000),
        ));
        server_store.put(chunk).unwrap();

        let mut client = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
        let mut server = swarm_with_store(server_store);

        let server_overlay = overlay(2);
        connect_and_activate(&mut client, &mut server, overlay(1), server_overlay).await;

        let (tx, rx) = oneshot::channel();
        client
            .behaviour_mut()
            .on_command(ClientCommand::RetrieveChunk {
                peer: server_overlay,
                address,
                response: tx,
            });

        let result = drive_until_retrieved(&mut client, &mut server, rx).await;
        assert!(
            result.is_err(),
            "an expired SOC must not be served; the stream resets so the requester forwards"
        );
    }

    #[tokio::test]
    async fn cache_miss_resets_with_stub_forwarder() {
        // The server's cache is empty and its forwarder is the stub, so the
        // inbound retrieval cannot be served or forwarded: the substream resets
        // and the requester sees a remote failure.
        let address = *content_chunk(b"never cached").address();

        let mut client = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
        let mut server = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));

        let server_overlay = overlay(2);
        connect_and_activate(&mut client, &mut server, overlay(1), server_overlay).await;

        let (tx, rx) = oneshot::channel();
        client
            .behaviour_mut()
            .on_command(ClientCommand::RetrieveChunk {
                peer: server_overlay,
                address,
                response: tx,
            });

        let result = drive_until_retrieved(&mut client, &mut server, rx).await;
        assert!(
            result.is_err(),
            "a cache miss with the stub forwarder must reset the stream"
        );
    }

    #[tokio::test]
    async fn inbound_pushsync_resets_with_stub_forwarder() {
        // The cache-only client never takes custody: an inbound pushsync forwards,
        // and with the stub forwarder the forward fails, so the substream resets
        // and the pusher sees a failure. No receipt is ever signed.
        let chunk = content_chunk(b"pushed chunk");

        let mut client = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
        let mut server = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));

        let server_overlay = overlay(2);
        connect_and_activate(&mut client, &mut server, overlay(1), server_overlay).await;

        let (tx, mut rx) = oneshot::channel();
        client.behaviour_mut().on_command(ClientCommand::PushChunk {
            peer: server_overlay,
            address: *chunk.address(),
            chunk,
            response: tx,
        });

        let drive = async {
            loop {
                tokio::select! {
                    _ = client.select_next_some() => {}
                    _ = server.select_next_some() => {}
                    res = &mut rx => return res.expect("sender not dropped"),
                }
            }
        };
        let result = tokio::time::timeout(Duration::from_secs(10), drive)
            .await
            .expect("push resolved within timeout");
        assert!(
            result.is_err(),
            "an inbound pushsync with the stub forwarder must reset the stream"
        );
    }
}
