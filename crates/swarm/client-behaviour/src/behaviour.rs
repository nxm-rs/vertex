//! `ClientBehaviour`: the client-side protocols (pricing, retrieval, pushsync)
//! driven through a per-connection [`ClientHandler`]. Handlers are created
//! dormant and activated after handshake completion.

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
use vertex_swarm_client_protocol::SwapEvent;
use vertex_swarm_client_protocol::{
    ChunkTransferError, ClientCommand, ClientEvent, PeerCommand, PeerEvent, PseudosettleEvent,
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

    /// Queue policy split: consumer events may drop at the cap (the swarm
    /// re-polls and their state is re-derivable), but handler commands never
    /// drop silently, because a dropped command strands state upstream: a lost
    /// retrieve or push cancels its responder while the dispatch-booked debit
    /// stands, a lost pseudosettle ack pins the handler's responder until the
    /// stale sweep, a lost activation leaves the handler dormant forever, and a
    /// lost settle or cheque strands the settlement service's pending entry.
    /// So the request commands (retrieve, push) refuse back to their caller at
    /// the cap, and every other command rides past it; each of those is
    /// connection- or trigger-rate bounded upstream, so the overrun is bounded.
    fn push_event(&mut self, event: ToSwarm<ClientEvent, HandlerCommand>) {
        if self.at_capacity() {
            warn!("Behaviour event queue full, dropping event");
            metrics::counter!("swarm.client.behaviour.events_dropped").increment(1);
            return;
        }
        self.pending_events.push_back(event);
    }

    /// True once the soft cap is reached; events drop and request commands
    /// refuse, per the policy on [`Self::push_event`].
    fn at_capacity(&self) -> bool {
        self.pending_events.len() >= self.config.max_pending_events
    }

    /// Enqueue a handler command regardless of the soft cap.
    fn push_command(&mut self, peer_id: PeerId, command: HandlerCommand) {
        self.pending_events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: libp2p::swarm::NotifyHandler::Any,
            event: command,
        });
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
                self.push_command(peer_id, HandlerCommand::Activate { overlay, node_type });
            }
            ClientCommand::Peer { peer, command } => {
                // A request command bearing a responder is refused at the soft
                // cap before any peer lookup, so an overloaded queue resolves the
                // caller rather than growing. Every other command rides past the
                // cap; an unknown peer refuses a responder-carrying command and
                // drops the rest.
                if command.is_request() && self.at_capacity() {
                    metrics::counter!("swarm.client.behaviour.commands_refused").increment(1);
                    command.refuse(ChunkTransferError::Overloaded);
                } else if let Some(&peer_id) = self.overlay_peers.get(&peer) {
                    self.push_command(peer_id, HandlerCommand::Peer(command));
                } else {
                    command.refuse(ChunkTransferError::NotConnected);
                }
            }
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
            HandlerEvent::Peer { overlay, event } => {
                self.tee_settlement(overlay, &event);
                // Inherited cap policy: a chunk delivery and a pricing-sent
                // notification ride past the soft cap; every other signal drops
                // at it.
                let cap_exempt = matches!(
                    event,
                    PeerEvent::ChunkReceived { .. } | PeerEvent::PricingSent
                );
                let to_swarm = ToSwarm::GenerateEvent(ClientEvent::Peer {
                    peer: overlay,
                    peer_id,
                    event,
                });
                if cap_exempt {
                    self.pending_events.push_back(to_swarm);
                } else {
                    self.push_event(to_swarm);
                }
            }
            HandlerEvent::Error {
                overlay,
                protocol,
                error,
            } => {
                // A dead settlement substream must release the service's pending
                // settle, else the settle oneshot leaks and the caller hangs past
                // its deadline.
                if let Some(peer) = overlay
                    && protocol == "pseudosettle"
                    && let Some(tx) = &self.pseudosettle_event_tx
                    && tx.send(PseudosettleEvent::Failed { peer }).is_err()
                {
                    warn!(%peer, "Pseudosettle event channel closed");
                }
                #[cfg(feature = "swap")]
                if let Some(peer) = overlay
                    && protocol == "swap"
                    && let Some(tx) = &self.swap_event_tx
                    && tx.send(SwapEvent::Failed { peer }).is_err()
                {
                    warn!(%peer, "Swap event channel closed");
                }
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

    /// Tee a settlement signal to its dedicated service channel. A dead channel
    /// warns but never blocks the event path; every non-settlement signal is a
    /// no-op here.
    fn tee_settlement(&self, overlay: OverlayAddress, event: &PeerEvent) {
        match event {
            PeerEvent::PseudosettleReceived { amount, request_id } => {
                if let Some(tx) = &self.pseudosettle_event_tx
                    && tx
                        .send(PseudosettleEvent::Received {
                            peer: overlay,
                            amount: *amount,
                            request_id: *request_id,
                        })
                        .is_err()
                {
                    warn!(%overlay, "Pseudosettle event channel closed");
                }
            }
            PeerEvent::PseudosettleSent { ack } => {
                if let Some(tx) = &self.pseudosettle_event_tx
                    && tx
                        .send(PseudosettleEvent::Sent {
                            peer: overlay,
                            ack: *ack,
                        })
                        .is_err()
                {
                    warn!(%overlay, "Pseudosettle event channel closed");
                }
            }
            #[cfg(feature = "swap")]
            PeerEvent::SwapChequeReceived { cheque, peer_rate } => {
                if let Some(tx) = &self.swap_event_tx
                    && tx
                        .send(SwapEvent::ChequeReceived {
                            peer: overlay,
                            cheque: cheque.clone(),
                            peer_rate: *peer_rate,
                        })
                        .is_err()
                {
                    warn!(%overlay, "Swap event channel closed");
                }
            }
            #[cfg(feature = "swap")]
            PeerEvent::SwapChequeSent { peer_rate } => {
                if let Some(tx) = &self.swap_event_tx
                    && tx
                        .send(SwapEvent::ChequeSent {
                            peer: overlay,
                            peer_rate: *peer_rate,
                        })
                        .is_err()
                {
                    warn!(%overlay, "Swap event channel closed");
                }
            }
            _ => {}
        }
    }
}

/// Refusal and classification helpers for a per-peer command.
///
/// Lives here rather than as an inherent impl on `PeerCommand` because the drop
/// log needs this crate's tracing and the command contract crate carries no
/// logging dependency.
trait PeerCommandExt {
    /// Whether the command carries a responder that a refusal must resolve.
    fn is_request(&self) -> bool;
    /// Resolve a request command's responder with `err`, or log-drop the rest.
    fn refuse(self, err: ChunkTransferError);
}

impl PeerCommandExt for PeerCommand {
    fn is_request(&self) -> bool {
        matches!(
            self,
            PeerCommand::RetrieveChunk { .. } | PeerCommand::PushChunk { .. }
        )
    }

    fn refuse(self, err: ChunkTransferError) {
        match self {
            PeerCommand::RetrieveChunk { response, .. } => {
                let _ = response.send(Err(err));
            }
            PeerCommand::PushChunk { response, .. } => {
                let _ = response.send(Err(err));
            }
            other => {
                debug!(
                    ?other,
                    ?err,
                    "Dropping command for unknown or overloaded peer"
                );
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
            // A full disconnect may never surface as a substream error, so
            // release any pending settle for this peer here too.
            if let Some(tx) = &self.pseudosettle_event_tx
                && tx
                    .send(PseudosettleEvent::Failed { peer: overlay })
                    .is_err()
            {
                warn!(%overlay, "Pseudosettle event channel closed");
            }
            #[cfg(feature = "swap")]
            if let Some(tx) = &self.swap_event_tx
                && tx.send(SwapEvent::Failed { peer: overlay }).is_err()
            {
                warn!(%overlay, "Swap event channel closed");
            }
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
    use libp2p::PeerId;
    use vertex_swarm_api::{ChunkAddress, SwarmResult};
    use vertex_swarm_primitives::CachedChunk;
    use vertex_swarm_test_utils::test_peer;

    use super::*;
    use crate::forward::StubForwarder;

    struct NoopStore;

    impl SwarmLocalStore for NoopStore {
        fn put(&self, _chunk: CachedChunk) -> SwarmResult<()> {
            Ok(())
        }
        fn get(&self, _address: &ChunkAddress) -> SwarmResult<Option<CachedChunk>> {
            Ok(None)
        }
        fn contains(&self, _address: &ChunkAddress) -> bool {
            false
        }
        fn remove(&self, _address: &ChunkAddress) -> SwarmResult<()> {
            Ok(())
        }
    }

    fn build_behaviour() -> ClientBehaviour {
        ClientBehaviour::new(
            Config::default(),
            Arc::new(NoopStore),
            Arc::new(StubForwarder),
        )
    }

    /// A behaviour whose event queue is permanently at capacity.
    fn saturated_behaviour() -> ClientBehaviour {
        let config = Config {
            max_pending_events: 0,
            ..Config::default()
        };
        ClientBehaviour::new(config, Arc::new(NoopStore), Arc::new(StubForwarder))
    }

    #[test]
    fn a_saturated_queue_refuses_a_retrieve_command_explicitly() {
        let mut behaviour = saturated_behaviour();
        let overlay = test_peer();
        // Activation rides past the cap (a dropped activation would leave the
        // handler dormant forever), so the peer is known when the request lands.
        behaviour.on_command(ClientCommand::ActivatePeer {
            peer_id: PeerId::random(),
            overlay,
            node_type: vertex_swarm_primitives::SwarmNodeType::Storer,
        });

        let (tx, mut rx) = tokio::sync::oneshot::channel();
        behaviour.on_command(ClientCommand::Peer {
            peer: overlay,
            command: PeerCommand::RetrieveChunk {
                address: ChunkAddress::zero(),
                response: tx,
                originated: true,
            },
        });

        match rx.try_recv() {
            Ok(Err(ChunkTransferError::Overloaded)) => {}
            other => panic!("expected an explicit Overloaded refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_saturated_queue_refuses_before_the_peer_lookup() {
        // The capacity check precedes the overlay lookup, so a request to an
        // unknown peer on a saturated queue refuses `Overloaded`, never
        // `NotConnected`.
        let mut behaviour = saturated_behaviour();

        let (tx, mut rx) = tokio::sync::oneshot::channel();
        behaviour.on_command(ClientCommand::Peer {
            peer: test_peer(),
            command: PeerCommand::RetrieveChunk {
                address: ChunkAddress::zero(),
                response: tx,
                originated: true,
            },
        });

        match rx.try_recv() {
            Ok(Err(ChunkTransferError::Overloaded)) => {}
            other => panic!("expected Overloaded before the peer lookup, got {other:?}"),
        }
    }

    #[test]
    fn commands_ride_past_the_cap_that_drops_events() {
        let mut behaviour = saturated_behaviour();
        let before = behaviour.pending_events.len();
        behaviour.on_command(ClientCommand::ActivatePeer {
            peer_id: PeerId::random(),
            overlay: test_peer(),
            node_type: vertex_swarm_primitives::SwarmNodeType::Storer,
        });
        assert_eq!(
            behaviour.pending_events.len(),
            before + 1,
            "an activation command enqueues past the soft cap"
        );

        // A droppable per-peer signal still drops at the cap through the envelope.
        behaviour.on_handler_event(
            PeerId::random(),
            HandlerEvent::Peer {
                overlay: test_peer(),
                event: PeerEvent::InboundServed,
            },
        );
        assert_eq!(
            behaviour.pending_events.len(),
            before + 1,
            "a droppable signal still drops at the cap"
        );

        // A chunk delivery is cap-exempt and enqueues even at the cap.
        let chunk: nectar_primitives::AnyChunk = nectar_primitives::ContentChunk::new(&b"cap"[..])
            .expect("valid content chunk")
            .into();
        behaviour.on_handler_event(
            PeerId::random(),
            HandlerEvent::Peer {
                overlay: test_peer(),
                event: PeerEvent::ChunkReceived {
                    address: ChunkAddress::zero(),
                    chunk,
                    stamp: None,
                    latency: std::time::Duration::ZERO,
                    originated: true,
                },
            },
        );
        assert_eq!(
            behaviour.pending_events.len(),
            before + 2,
            "a chunk delivery rides past the cap through the envelope"
        );
    }

    #[test]
    fn pseudosettle_substream_error_routes_failed() {
        let mut behaviour = build_behaviour();
        let (tx, mut rx) = mpsc::unbounded_channel();
        behaviour.route_pseudosettle_events(tx);

        let peer = test_peer();
        behaviour.on_handler_event(
            PeerId::random(),
            HandlerEvent::Error {
                overlay: Some(peer),
                protocol: "pseudosettle",
                error: "substream timed out".into(),
            },
        );

        assert!(matches!(
            rx.try_recv(),
            Ok(PseudosettleEvent::Failed { peer: p }) if p == peer
        ));
    }

    #[test]
    fn non_settlement_error_does_not_route_failed() {
        let mut behaviour = build_behaviour();
        let (tx, mut rx) = mpsc::unbounded_channel();
        behaviour.route_pseudosettle_events(tx);

        behaviour.on_handler_event(
            PeerId::random(),
            HandlerEvent::Error {
                overlay: Some(test_peer()),
                protocol: "pricing",
                error: "boom".into(),
            },
        );

        assert!(rx.try_recv().is_err());
    }

    #[cfg(feature = "swap")]
    #[test]
    fn swap_substream_error_routes_failed() {
        let mut behaviour = build_behaviour();
        let (tx, mut rx) = mpsc::unbounded_channel();
        behaviour.route_swap_events(tx);

        let peer = test_peer();
        behaviour.on_handler_event(
            PeerId::random(),
            HandlerEvent::Error {
                overlay: Some(peer),
                protocol: "swap",
                error: "substream timed out".into(),
            },
        );

        assert!(matches!(
            rx.try_recv(),
            Ok(SwapEvent::Failed { peer: p }) if p == peer
        ));
    }
}
