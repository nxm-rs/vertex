//! Connection handler for client protocols (pricing, retrieval, pushsync,
//! pseudosettle, and swap when enabled) on a single peer connection.
//!
//! The handler is `Dormant` until an `Activate` command (sent after handshake)
//! transitions it to `Active`, after which it processes protocol messages.
//!
//! Retrieval and pushsync inbound requests are served by self-contained futures
//! in the `inbound` set, each resolving to an [`InboundOutcome`] the handler
//! turns into a scoring or metrics event; the response is sent inside the future,
//! never routed back as a command. Pseudosettle inbound uses the request-id
//! responder map instead, because its ack is gated on a time-based allowance and
//! cannot be folded inline.

use std::{
    collections::HashMap,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use vertex_tasks::time::Instant;

use alloy_primitives::U256;
use futures_bounded::Timeout;
use libp2p::swarm::{
    SubstreamProtocol,
    handler::{
        ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
        FullyNegotiatedOutbound,
    },
};
use nectar_primitives::{ChunkAddress, NetworkId};
use tracing::{debug, warn};
use vertex_swarm_api::{Au, SwarmLocalStore};
use vertex_swarm_net_handler_core::{BoundedQueue, OutcomeDriver};
use vertex_swarm_net_pseudosettle::PaymentAck;
use vertex_swarm_net_pushsync::Receipt;
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

use super::events::{PushResponseTx, RetrievalResponseTx};
use super::forward::Forwarder;
use super::serve::{self, PushServe, RetrieveServe};
use super::storer::StorerCapability;
use super::upgrade::{
    ClientInboundOutput, ClientInboundUpgrade, ClientOutboundInfo, ClientOutboundOutput,
    ClientOutboundUpgrade, ClientUpgradeError, FailureKind,
};
use vertex_swarm_client_protocol::{
    ChunkTransferError, PeerCommand, PeerEvent, PseudosettleAck, RetrievalResult,
};
use vertex_swarm_net_pushsync::PROTOCOL_NAME as PUSHSYNC_PROTOCOL;
use vertex_swarm_net_retrieval::PROTOCOL_NAME as RETRIEVAL_PROTOCOL;

const DEFAULT_MAX_PENDING_COMMANDS: usize = 256;
const DEFAULT_MAX_PENDING_EVENTS: usize = 256;
/// Maximum number of stored pseudosettle responders per connection.
const MAX_PENDING_RESPONSES: usize = 64;
/// Timeout for async response sending (prevent stuck streams).
const RESPONSE_SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum concurrent response sends per connection.
const MAX_CONCURRENT_RESPONSE_SENDS: usize = 8;
/// Responders older than this are dropped as stale.
const RESPONDER_STALE_TIMEOUT: Duration = Duration::from_secs(60);
/// Maximum concurrent inbound serving futures per connection. Once full,
/// `listen_protocol` stops advertising inbound serving so the muxer
/// back-pressures the peer.
const MAX_INBOUND_SERVING: usize = 32;

/// Outcome of serving one inbound retrieval or pushsync request. The response is
/// already sent (or the substream reset) inside the future; this carries only the
/// scoring/metrics signal.
#[derive(Debug)]
pub(crate) enum InboundOutcome {
    /// Retrieval answered from cache.
    Served { overlay: OverlayAddress },
    /// Retrieval answered by forwarding to a closer peer.
    Forwarded { overlay: OverlayAddress },
    /// Retrieval could not be served or forwarded; substream reset.
    Missed {
        overlay: OverlayAddress,
        address: ChunkAddress,
    },
    /// Pushsync forwarded and the storer's receipt relayed verbatim.
    Relayed { overlay: OverlayAddress },
    /// Pushsync the node is responsible for: stored into the reserve and
    /// acknowledged with a freshly signed custody receipt.
    Stored { overlay: OverlayAddress },
    /// Pushsync could not be forwarded, stored, or acknowledged; substream reset.
    PushFailed {
        overlay: OverlayAddress,
        address: ChunkAddress,
    },
}

/// Configuration for the client handler.
///
/// The three deadlines are separate fields on purpose: `retrieval_timeout` and
/// `pushsync_timeout` bound each chunk-transfer substream upgrade, including the
/// blocked read of the response frame, so a peer that negotiates the substream
/// then withholds the response resolves with [`ChunkTransferError::TimedOut`]
/// rather than stalling the caller. This is the only liveness boundary against a
/// withholding peer. Do not collapse them into the shared `timeout` (used by
/// pricing, pseudosettle, and swap); tuning one must not move settlement.
#[derive(Debug, Clone)]
pub struct Config {
    /// Shared deadline for pricing, pseudosettle, and swap.
    pub timeout: Duration,
    /// Outbound retrieval deadline; see the type-level note.
    pub retrieval_timeout: Duration,
    /// Outbound pushsync deadline; see the type-level note.
    pub pushsync_timeout: Duration,
    pub max_pending_commands: usize,
    pub max_pending_events: usize,
    /// Controls which protocols are advertised on inbound upgrades and which
    /// outbound commands are honoured. Bootnodes only speak pricing.
    pub local_role: SwarmNodeType,
    /// Used to recover the signer overlay of an inbound custody receipt at decode
    /// (`compute_overlay(eth, network_id, nonce)`).
    pub network_id: NetworkId,
    /// Advertised swap exchange rate sent in the swap headers exchange.
    #[cfg(feature = "swap")]
    pub swap_exchange_rate: U256,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            retrieval_timeout: Duration::from_secs(30),
            pushsync_timeout: Duration::from_secs(30),
            max_pending_commands: DEFAULT_MAX_PENDING_COMMANDS,
            max_pending_events: DEFAULT_MAX_PENDING_EVENTS,
            local_role: SwarmNodeType::Client,
            network_id: NetworkId::MAINNET,
            #[cfg(feature = "swap")]
            swap_exchange_rate: U256::ZERO,
        }
    }
}

/// Commands sent from the behaviour to the handler.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum HandlerCommand {
    /// Activate the handler after handshake completion.
    Activate {
        overlay: OverlayAddress,
        node_type: SwarmNodeType,
    },
    /// A per-peer command; the connection already identifies the peer.
    Peer(PeerCommand),
}

/// Events emitted by the handler to the behaviour.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum HandlerEvent {
    /// Handler has been activated.
    Activated { overlay: OverlayAddress },
    /// A per-peer signal under the connection's active overlay.
    Peer {
        overlay: OverlayAddress,
        event: PeerEvent,
    },
    /// Protocol error occurred.
    Error {
        overlay: Option<OverlayAddress>,
        protocol: &'static str,
        error: String,
    },
}

impl From<InboundOutcome> for HandlerEvent {
    fn from(outcome: InboundOutcome) -> Self {
        match outcome {
            InboundOutcome::Served { overlay } => HandlerEvent::Peer {
                overlay,
                event: PeerEvent::InboundServed,
            },
            InboundOutcome::Forwarded { overlay } => HandlerEvent::Peer {
                overlay,
                event: PeerEvent::InboundForwarded,
            },
            InboundOutcome::Missed { overlay, address } => HandlerEvent::Peer {
                overlay,
                event: PeerEvent::InboundMissed { address },
            },
            InboundOutcome::Relayed { overlay } => HandlerEvent::Peer {
                overlay,
                event: PeerEvent::InboundRelayed,
            },
            InboundOutcome::Stored { overlay } => HandlerEvent::Peer {
                overlay,
                event: PeerEvent::InboundStored,
            },
            InboundOutcome::PushFailed { overlay, address } => HandlerEvent::Peer {
                overlay,
                event: PeerEvent::InboundPushFailed { address },
            },
        }
    }
}

/// Handler state machine.
#[derive(Debug)]
enum State {
    /// Waiting for activation command.
    Dormant,
    /// Active and processing protocols.
    Active { overlay: OverlayAddress },
}

/// A pending inbound pseudosettle response awaiting the application's ack.
struct StoredResponse {
    response: vertex_swarm_net_pseudosettle::PseudosettleInboundResult,
    stored_at: Instant,
}

/// Swarm client connection handler managing multiple client protocols on a
/// single peer connection.
pub struct ClientHandler {
    config: Config,
    state: State,
    /// Client cache: inbound retrievals serve from it, forwarded deliveries cache
    /// into it.
    store: Arc<dyn SwarmLocalStore>,
    /// Forwards a retrieval cache miss or a pushsync this node is not responsible
    /// for. Stubbed in the cache-only client.
    forward: Arc<dyn Forwarder>,
    /// Present only on a storer node. When set, an inbound pushsync the node is
    /// responsible for is stored and acknowledged with a signed custody receipt;
    /// when absent, every delivery takes the verbatim-relay path.
    storer: Option<StorerCapability>,
    next_request_id: u64,
    pending_commands: BoundedQueue<HandlerCommand>,
    pending_events: BoundedQueue<HandlerEvent>,
    /// One announce substream in flight at a time; each announcement (initial
    /// and checkpoint re-announcements) opens a fresh substream.
    announce_in_flight: bool,
    /// Latest outbound announcement superseded while one is in flight; sent
    /// once the in-flight substream resolves. Raises are monotonic, so only
    /// the newest line matters.
    superseded_announce: Option<U256>,
    /// Self-contained inbound serving futures (retrieval and pushsync).
    inbound: OutcomeDriver<InboundOutcome>,
    /// Pseudosettle responders awaiting the service's ack, keyed by request_id.
    /// Only pseudosettle uses this, because its ack is gated on a time-based
    /// allowance.
    pending_responses: HashMap<u64, StoredResponse>,
    /// Bounded set for async pseudosettle ack sends (prevents blocking poll).
    response_sends: futures_bounded::FuturesSet<Result<(), String>>,
    /// Latest payment threshold received while dormant, flushed once the
    /// handler activates. A peer announces at connect, before the overlay
    /// round-trips through activation, so without this the connect-time
    /// announcement is dropped and never reaches accounting.
    pending_peer_threshold: Option<U256>,
}

impl ClientHandler {
    /// Push an event if the queue isn't full, otherwise drop with a metric.
    fn push_event(&mut self, event: HandlerEvent) {
        if self.pending_events.push(event).is_err() {
            warn!("Handler event queue full, dropping event");
            metrics::counter!("swarm_client_handler_events_dropped_total").increment(1);
        }
    }

    /// Create a new handler in dormant state. `storer` is `Some` only on a storer
    /// node; a client passes `None` and runs the verbatim-relay pushsync path.
    pub(crate) fn new(
        config: Config,
        store: Arc<dyn SwarmLocalStore>,
        forward: Arc<dyn Forwarder>,
        storer: Option<StorerCapability>,
    ) -> Self {
        let max_pending_commands = config.max_pending_commands;
        let max_pending_events = config.max_pending_events;
        Self {
            config,
            state: State::Dormant,
            store,
            forward,
            storer,
            next_request_id: 0,
            pending_commands: BoundedQueue::new(max_pending_commands),
            pending_events: BoundedQueue::new(max_pending_events),
            announce_in_flight: false,
            superseded_announce: None,
            inbound: OutcomeDriver::new(MAX_INBOUND_SERVING),
            pending_responses: HashMap::new(),
            response_sends: futures_bounded::FuturesSet::new(
                RESPONSE_SEND_TIMEOUT,
                MAX_CONCURRENT_RESPONSE_SENDS,
            ),
            pending_peer_threshold: None,
        }
    }

    fn overlay(&self) -> Option<OverlayAddress> {
        match &self.state {
            State::Active { overlay, .. } => Some(*overlay),
            _ => None,
        }
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        id
    }

    /// Store a pending pseudosettle response, evicting stale entries (then the
    /// oldest) when at capacity.
    fn store_response(
        &mut self,
        request_id: u64,
        response: vertex_swarm_net_pseudosettle::PseudosettleInboundResult,
    ) {
        if self.pending_responses.len() >= MAX_PENDING_RESPONSES {
            self.evict_stale_responses();
        }
        if self.pending_responses.len() >= MAX_PENDING_RESPONSES {
            warn!(%request_id, "Pending response map full, dropping oldest");
            metrics::counter!("swarm_client_handler_responses_dropped_total").increment(1);
            if let Some(&oldest_id) = self
                .pending_responses
                .iter()
                .min_by_key(|(_, v)| v.stored_at)
                .map(|(k, _)| k)
            {
                self.pending_responses.remove(&oldest_id);
            }
        }
        self.pending_responses.insert(
            request_id,
            StoredResponse {
                response,
                stored_at: Instant::now(),
            },
        );
    }

    fn evict_stale_responses(&mut self) {
        // checked_sub: early in a paused-clock test the clock may not yet have
        // advanced past the timeout, and tokio instants panic on underflow.
        let Some(cutoff) = Instant::now().checked_sub(RESPONDER_STALE_TIMEOUT) else {
            return;
        };
        self.pending_responses.retain(|_, v| v.stored_at > cutoff);
    }

    fn take_response(
        &mut self,
        request_id: u64,
    ) -> Option<vertex_swarm_net_pseudosettle::PseudosettleInboundResult> {
        self.pending_responses
            .remove(&request_id)
            .map(|s| s.response)
    }

    /// Re-enqueue an announcement superseded while an announce substream was in
    /// flight, so the newest line still reaches the peer.
    fn requeue_superseded_announce(&mut self) {
        if let Some(threshold) = self.superseded_announce.take()
            && self
                .pending_commands
                .push(HandlerCommand::Peer(
                    PeerCommand::AnnouncePaymentThreshold { threshold },
                ))
                .is_err()
        {
            warn!("Handler command queue full, dropping superseded announcement");
            metrics::counter!("swarm_client_handler_commands_dropped_total").increment(1);
        }
    }

    fn activate(&mut self, overlay: OverlayAddress, node_type: SwarmNodeType) {
        match &self.state {
            State::Dormant => {
                debug!(%overlay, ?node_type, "Handler activated");
                self.state = State::Active { overlay };
                self.pending_events
                    .push_back(HandlerEvent::Activated { overlay });
                // Flush a payment threshold that arrived before activation, so
                // a peer that announces at connect still reaches accounting.
                if let Some(threshold) = self.pending_peer_threshold.take() {
                    debug!(%overlay, %threshold, "Flushing buffered payment threshold");
                    self.pending_events.push_back(HandlerEvent::Peer {
                        overlay,
                        event: PeerEvent::PaymentThresholdReceived { threshold },
                    });
                }
            }
            State::Active { .. } => {
                warn!("Handler already active, ignoring duplicate activation");
            }
        }
    }

    /// Handle an incoming payment threshold.
    fn on_payment_threshold_received(
        &mut self,
        threshold: vertex_swarm_net_pricing::AnnouncePaymentThreshold,
    ) {
        if let Some(overlay) = self.overlay() {
            debug!(%overlay, threshold = %threshold.payment_threshold, "Received payment threshold");
            self.pending_events.push_back(HandlerEvent::Peer {
                overlay,
                event: PeerEvent::PaymentThresholdReceived {
                    threshold: threshold.payment_threshold,
                },
            });
        } else {
            debug!(
                threshold = %threshold.payment_threshold,
                "Buffering payment threshold received before activation"
            );
            self.pending_peer_threshold = Some(threshold.payment_threshold);
        }
    }

    /// Serve an inbound retrieval from a self-contained future: cache hit (content
    /// indefinitely, single-owner while fresh), else forward to a closer peer.
    fn on_retrieval_request(
        &mut self,
        request: vertex_swarm_net_retrieval::Request,
        responder: vertex_swarm_net_retrieval::RetrievalResponder,
    ) {
        let Some(overlay) = self.overlay() else {
            warn!(
                address = %request.address,
                "Received retrieval request in dormant state (peer may have cached old protocol list)"
            );
            return;
        };
        let address = request.address;
        debug!(%overlay, %address, "Received retrieval request");

        let op = RetrieveServe {
            store: Arc::clone(&self.store),
            forward: Arc::clone(&self.forward),
            overlay,
            address,
        };
        self.inbound.push(Box::pin(serve::drive(op, responder)));
    }

    /// Handle an inbound pushsync delivery.
    ///
    /// A storer responsible for the chunk takes custody (store and sign).
    /// Otherwise the delivery is forwarded to a closer peer and the storer's
    /// receipt relayed verbatim; this node never signs for a chunk it does not
    /// store. A store or forward failure resets the substream.
    fn on_pushsync_delivery(
        &mut self,
        delivery: vertex_swarm_net_pushsync::Delivery,
        responder: vertex_swarm_net_pushsync::PushsyncResponder,
    ) {
        let Some(overlay) = self.overlay() else {
            warn!(
                address = %delivery.chunk.address(),
                "Received pushsync delivery in dormant state (peer may have cached old protocol list)"
            );
            return;
        };
        let chunk = *delivery.chunk;
        let address = *chunk.address();
        debug!(%overlay, %address, "Received pushsync delivery");

        let op = PushServe {
            storer: self.storer.clone(),
            forward: Arc::clone(&self.forward),
            overlay,
            chunk,
        };
        self.inbound.push(Box::pin(serve::drive(op, responder)));
    }

    /// Handle retrieval response, resolving the caller's response channel.
    fn on_retrieval_response(
        &mut self,
        delivery: vertex_swarm_net_retrieval::Delivery,
        address: ChunkAddress,
        response: RetrievalResponseTx,
        latency: Duration,
        originated: bool,
    ) {
        let overlay = self.overlay();
        match delivery {
            vertex_swarm_net_retrieval::Delivery::Error => {
                // Explicit error delivery: the peer signalled absence before
                // charging, the one retrieval outcome that provably moved no
                // bytes, so surface `NotFound` to release the origin reservation.
                // Malformed chunks never reach here; they fail decode and surface
                // as a dial upgrade error.
                debug!(?overlay, %address, "Retrieval failed");
                if let Some(overlay) = overlay {
                    self.push_event(HandlerEvent::Peer {
                        overlay,
                        event: PeerEvent::RetrievalFailed {
                            address,
                            error: "remote reported a failure".to_string(),
                            kind: FailureKind::Protocol,
                        },
                    });
                }
                let _ = response.send(Err(ChunkTransferError::NotFound(address)));
            }
            vertex_swarm_net_retrieval::Delivery::Chunk { chunk, stamp } => {
                let chunk = *chunk;
                let Some(overlay) = overlay else {
                    let _ = response.send(Err(ChunkTransferError::Protocol(
                        "handler not active".to_string(),
                    )));
                    return;
                };
                debug!(%overlay, %address, "Received chunk");
                self.push_event(HandlerEvent::Peer {
                    overlay,
                    event: PeerEvent::ChunkReceived {
                        address,
                        chunk: chunk.clone(),
                        stamp: stamp.clone(),
                        latency,
                        originated,
                    },
                });
                let delivered = response.send(Ok(RetrievalResult {
                    chunk,
                    stamp,
                    peer: overlay,
                }));
                // An originated delivery whose receiver is gone: the attempt that
                // asked for it already lost (or the request was abandoned), so the
                // chunk was fetched and metered but is discarded. Retrieval is not
                // cancellable downstream, so this is the delivery-side over-fetch
                // the staggered race trades for failover latency.
                if originated && delivered.is_err() {
                    metrics::counter!("swarm_client_retrieval_overfetch_delivered_total")
                        .increment(1);
                }
            }
        }
    }

    /// Handle pushsync receipt, resolving the caller's response channel.
    fn on_pushsync_receipt(
        &mut self,
        response_msg: vertex_swarm_net_pushsync::ReceiptResponse,
        address: ChunkAddress,
        response: PushResponseTx,
        latency: Duration,
        originated: bool,
    ) {
        let overlay = self.overlay();
        match response_msg {
            vertex_swarm_net_pushsync::ReceiptResponse::Failed => {
                // Remote reported a rejection (empty signature).
                debug!(?overlay, %address, "Pushsync failed");
                if let Some(overlay) = overlay {
                    self.push_event(HandlerEvent::Peer {
                        overlay,
                        event: PeerEvent::PushFailed {
                            address,
                            error: "remote reported a failure".to_string(),
                            kind: FailureKind::Protocol,
                        },
                    });
                }
                let _ = response.send(Err(ChunkTransferError::Remote));
            }
            vertex_swarm_net_pushsync::ReceiptResponse::Stored(receipt) => {
                let Some(overlay) = overlay else {
                    let _ = response.send(Err(ChunkTransferError::Protocol(
                        "handler not active".to_string(),
                    )));
                    return;
                };
                let receipt_address = receipt.address;
                // Decode boundary: reconstruct and verify the receipt storer
                // before any consumer sees it. An unrecoverable signature is
                // rejected here as invalid data and the peer is scored.
                match Receipt::reconstruct(receipt, self.config.network_id) {
                    Ok(receipt) => {
                        debug!(%overlay, address = %receipt_address, "Received receipt");
                        self.push_event(HandlerEvent::Peer {
                            overlay,
                            event: PeerEvent::ReceiptReceived {
                                address: receipt_address,
                                latency,
                                originated,
                            },
                        });
                        let _ = response.send(Ok(receipt));
                    }
                    Err(err) => {
                        debug!(
                            %overlay,
                            address = %receipt_address,
                            error = <&'static str>::from(&err),
                            "Rejected unrecoverable custody receipt at decode"
                        );
                        self.push_event(HandlerEvent::Peer {
                            overlay,
                            event: PeerEvent::PushFailed {
                                address: receipt_address,
                                error: err.to_string(),
                                kind: FailureKind::InvalidChunk,
                            },
                        });
                        let _ = response.send(Err(ChunkTransferError::Remote));
                    }
                }
            }
        }
    }
}

#[allow(deprecated)]
impl ConnectionHandler for ClientHandler {
    type FromBehaviour = HandlerCommand;
    type ToBehaviour = HandlerEvent;
    type InboundProtocol = ClientInboundUpgrade;
    type OutboundProtocol = ClientOutboundUpgrade;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ClientOutboundInfo;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        // Back-pressure: once the inbound serving set is full, advertise the
        // dormant (empty) protocol set so the muxer stops accepting new inbound
        // substreams until we drain.
        let upgrade = match &self.state {
            State::Active { .. } if self.inbound.has_capacity() => {
                let upgrade = ClientInboundUpgrade::active_for(self.config.local_role);
                #[cfg(feature = "swap")]
                let upgrade = upgrade.with_swap_rate(self.config.swap_exchange_rate);
                upgrade
            }
            State::Active { .. } | State::Dormant => ClientInboundUpgrade::new(),
        };
        SubstreamProtocol::new(upgrade, ()).with_timeout(self.config.timeout)
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        if let Some(event) = self.pending_events.pop() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Drain resolved inbound serving futures into scoring/metrics events.
        while let Poll::Ready(Some(outcome)) = self.inbound.poll_next(cx) {
            self.push_event(outcome.into());
            if let Some(event) = self.pending_events.pop() {
                return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
            }
        }

        // Drain completed pseudosettle ack sends.
        while let Poll::Ready(result) = self.response_sends.poll_unpin(cx) {
            match result {
                Ok(Ok(())) => {
                    debug!("Response send completed");
                }
                Ok(Err(err)) => {
                    warn!(error = %err, "Response send failed");
                    self.push_event(HandlerEvent::Error {
                        overlay: self.overlay(),
                        protocol: "response",
                        error: err,
                    });
                }
                Err(Timeout { .. }) => {
                    warn!("Response send timed out");
                    self.push_event(HandlerEvent::Error {
                        overlay: self.overlay(),
                        protocol: "response",
                        error: "response send timed out".into(),
                    });
                }
            }
            if let Some(event) = self.pending_events.pop() {
                return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
            }
        }

        while let Some(cmd) = self.pending_commands.pop() {
            match cmd {
                HandlerCommand::Activate { overlay, node_type } => {
                    self.activate(overlay, node_type);
                    if let Some(event) = self.pending_events.pop() {
                        return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
                    }
                }
                HandlerCommand::Peer(command) => match command {
                    PeerCommand::AnnouncePaymentThreshold { threshold } => {
                        if self.announce_in_flight {
                            self.superseded_announce = Some(threshold);
                        } else {
                            self.announce_in_flight = true;
                            let announce =
                                vertex_swarm_net_pricing::AnnouncePaymentThreshold::new(threshold);
                            let upgrade = ClientOutboundUpgrade::pricing(announce);
                            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                                protocol: SubstreamProtocol::new(
                                    upgrade,
                                    ClientOutboundInfo::Pricing,
                                )
                                .with_timeout(self.config.timeout),
                            });
                        }
                    }
                    PeerCommand::RetrieveChunk {
                        address,
                        response,
                        originated,
                    } => {
                        let upgrade = ClientOutboundUpgrade::retrieval(address);
                        return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                            protocol: SubstreamProtocol::new(
                                upgrade,
                                ClientOutboundInfo::Retrieval {
                                    address,
                                    response,
                                    requested_at: Instant::now(),
                                    originated,
                                },
                            )
                            .with_timeout(self.config.retrieval_timeout),
                        });
                    }
                    PeerCommand::PushChunk {
                        chunk,
                        response,
                        originated,
                    } => {
                        let address = *chunk.address();
                        let delivery = vertex_swarm_net_pushsync::Delivery::new(chunk);
                        let upgrade = ClientOutboundUpgrade::pushsync(delivery);
                        return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                            protocol: SubstreamProtocol::new(
                                upgrade,
                                ClientOutboundInfo::Pushsync {
                                    address,
                                    response,
                                    requested_at: Instant::now(),
                                    originated,
                                },
                            )
                            .with_timeout(self.config.pushsync_timeout),
                        });
                    }
                    PeerCommand::SendPseudosettle { amount } => {
                        let payment = vertex_swarm_net_pseudosettle::Payment::new(amount);
                        let upgrade = ClientOutboundUpgrade::pseudosettle(payment);
                        return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                            protocol: SubstreamProtocol::new(
                                upgrade,
                                ClientOutboundInfo::Pseudosettle { amount },
                            )
                            .with_timeout(self.config.timeout),
                        });
                    }
                    #[cfg(feature = "swap")]
                    PeerCommand::SendCheque { cheque } => {
                        let upgrade =
                            ClientOutboundUpgrade::swap(cheque, self.config.swap_exchange_rate);
                        return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                            protocol: SubstreamProtocol::new(upgrade, ClientOutboundInfo::Swap)
                                .with_timeout(self.config.timeout),
                        });
                    }
                    PeerCommand::AckPseudosettle { request_id, ack } => {
                        if let Some(result) = self.take_response(request_id) {
                            // Convert the domain decision to the wire ack at the
                            // boundary; the responder's sampled clock passes
                            // through unchanged.
                            let ack = wire_ack(ack);
                            debug!(%request_id, amount = %ack.amount, "Sending pseudosettle ack");
                            if self
                                .response_sends
                                .try_push(async move {
                                    result
                                        .respond(ack)
                                        .await
                                        .map_err(|e| format!("pseudosettle ack: {e}"))
                                })
                                .is_err()
                            {
                                warn!("Response send queue full, dropping pseudosettle ack");
                            }
                        } else {
                            warn!(%request_id, "No pseudosettle responder found for request_id");
                        }
                    }
                    // `PeerCommand` carries the swap variant when
                    // `client-protocol/swap` is on, which Cargo feature
                    // unification can turn on (a workspace build also compiling
                    // `accounting-swap`) even when this crate's `swap` feature is
                    // off. The swap wire is then not linked here, so drop the
                    // command. The all-features build keeps full exhaustiveness.
                    // Unreachable when nothing in the build enables
                    // `client-protocol/swap`.
                    #[cfg(not(feature = "swap"))]
                    #[allow(unreachable_patterns)]
                    _ => {}
                },
            }
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        if self.pending_commands.push(event).is_err() {
            warn!("Handler command queue full, dropping command");
            metrics::counter!("swarm_client_handler_commands_dropped_total").increment(1);
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
                protocol: output,
                ..
            }) => {
                self.handle_inbound_output(output);
            }

            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: output,
                info,
                ..
            }) => {
                self.handle_outbound_output(output, info);
            }

            ConnectionEvent::DialUpgradeError(e) => {
                // Classify from the typed error while concrete: a malformed chunk
                // arrives as an `Apply` error we downcast, not a parsed string.
                let apply_error = match &e.error {
                    libp2p::swarm::StreamUpgradeError::Apply(err) => Some(err),
                    _ => None,
                };
                // Timeout means the per-protocol deadline fired: the substream
                // negotiated but the response frame never arrived. The
                // chunk-transfer arms resolve the caller with the typed
                // `ChunkTransferError::TimedOut` while still scoring as
                // `FailureKind::Protocol`.
                let timed_out = matches!(&e.error, libp2p::swarm::StreamUpgradeError::Timeout);
                let error = e.error.to_string();
                match e.info {
                    ClientOutboundInfo::Pricing => {
                        self.announce_in_flight = false;
                        self.requeue_superseded_announce();
                        warn!(protocol = "pricing", %error, "Client dial upgrade error");
                        self.push_event(HandlerEvent::Error {
                            overlay: self.overlay(),
                            protocol: "pricing",
                            error,
                        });
                    }
                    ClientOutboundInfo::Retrieval {
                        address,
                        response,
                        requested_at,
                        originated: _,
                    } => {
                        // A timeout is never a malformed chunk; an `Apply` error
                        // may be a malformed delivery.
                        let kind = apply_error
                            .map_or(FailureKind::Protocol, |e| e.retrieval_failure_kind());
                        if timed_out {
                            // Sole emission site for the retrieval timeout counter.
                            metrics::counter!("swarm_client_retrieval_timeouts_total").increment(1);
                            debug!(
                                peer_overlay = ?self.overlay(),
                                %address,
                                elapsed = ?requested_at.elapsed(),
                                "Retrieval timed out waiting on a withholding peer"
                            );
                        }
                        warn!(protocol = "retrieval", %address, %error, ?kind, "Client dial upgrade error");
                        if let Some(overlay) = self.overlay() {
                            self.push_event(HandlerEvent::Peer {
                                overlay,
                                event: PeerEvent::RetrievalFailed {
                                    address,
                                    error: error.clone(),
                                    kind,
                                },
                            });
                        }
                        let outcome = if timed_out {
                            ChunkTransferError::TimedOut
                        } else {
                            ChunkTransferError::Protocol(error)
                        };
                        let _ = response.send(Err(outcome));
                    }
                    ClientOutboundInfo::Pushsync {
                        address,
                        response,
                        requested_at,
                        originated: _,
                    } => {
                        let kind = apply_error
                            .map_or(FailureKind::Protocol, |e| e.pushsync_failure_kind());
                        if timed_out {
                            // Sole emission site for the pushsync timeout counter.
                            metrics::counter!("swarm_client_pushsync_timeouts_total").increment(1);
                            debug!(
                                peer_overlay = ?self.overlay(),
                                %address,
                                elapsed = ?requested_at.elapsed(),
                                "Pushsync timed out waiting on a withholding peer"
                            );
                        }
                        warn!(protocol = "pushsync", %address, %error, ?kind, "Client dial upgrade error");
                        if let Some(overlay) = self.overlay() {
                            self.push_event(HandlerEvent::Peer {
                                overlay,
                                event: PeerEvent::PushFailed {
                                    address,
                                    error: error.clone(),
                                    kind,
                                },
                            });
                        }
                        let outcome = if timed_out {
                            ChunkTransferError::TimedOut
                        } else {
                            ChunkTransferError::Protocol(error)
                        };
                        let _ = response.send(Err(outcome));
                    }
                    ClientOutboundInfo::Pseudosettle { .. } => {
                        warn!(protocol = "pseudosettle", %error, "Client dial upgrade error");
                        self.push_event(HandlerEvent::Error {
                            overlay: self.overlay(),
                            protocol: "pseudosettle",
                            error,
                        });
                    }
                    #[cfg(feature = "swap")]
                    ClientOutboundInfo::Swap => {
                        warn!(protocol = "swap", %error, "Client dial upgrade error");
                        self.push_event(HandlerEvent::Error {
                            overlay: self.overlay(),
                            protocol: "swap",
                            error,
                        });
                    }
                }
            }

            ConnectionEvent::ListenUpgradeError(e) => {
                // A malformed inbound chunk or retrieval request fails
                // reconstruction at decode and surfaces here; classify so the
                // offending peer is scored. The chunk is already rejected.
                let kind = e.error.inbound_failure_kind();
                warn!(error = %e.error, ?kind, "Client listen upgrade error");
                match (kind, self.overlay()) {
                    (FailureKind::InvalidChunk, Some(overlay)) => {
                        let protocol = match &e.error {
                            ClientUpgradeError::Pushsync(_) => PUSHSYNC_PROTOCOL,
                            ClientUpgradeError::Retrieval(_) => RETRIEVAL_PROTOCOL,
                            _ => "unknown",
                        };
                        self.push_event(HandlerEvent::Peer {
                            overlay,
                            event: PeerEvent::InboundInvalidData { protocol },
                        });
                    }
                    _ => {
                        self.push_event(HandlerEvent::Error {
                            overlay: self.overlay(),
                            protocol: "unknown",
                            error: e.error.to_string(),
                        });
                    }
                }
            }

            _ => {}
        }
    }
}

impl ClientHandler {
    fn handle_inbound_output(&mut self, output: ClientInboundOutput) {
        match output {
            ClientInboundOutput::Pricing(threshold) => {
                self.on_payment_threshold_received(threshold);
            }
            ClientInboundOutput::Retrieval(request, responder) => {
                self.on_retrieval_request(request, responder);
            }
            ClientInboundOutput::Pushsync(delivery, responder) => {
                self.on_pushsync_delivery(delivery, responder);
            }
            ClientInboundOutput::Pseudosettle(result) => {
                if let Some(overlay) = self.overlay() {
                    let request_id = self.next_request_id();
                    debug!(%overlay, amount = %result.payment.amount, %request_id, "Received pseudosettle payment");
                    self.pending_events.push_back(HandlerEvent::Peer {
                        overlay,
                        event: PeerEvent::PseudosettleReceived {
                            amount: result.payment.amount,
                            request_id,
                        },
                    });
                    self.store_response(request_id, result);
                }
            }
            #[cfg(feature = "swap")]
            ClientInboundOutput::Swap(cheque, headers) => {
                if let Some(overlay) = self.overlay() {
                    debug!(%overlay, peer_rate = %headers.exchange_rate, "Received swap cheque");
                    self.push_event(HandlerEvent::Peer {
                        overlay,
                        event: PeerEvent::SwapChequeReceived {
                            cheque,
                            peer_rate: headers.exchange_rate,
                        },
                    });
                }
            }
        }
    }

    fn handle_outbound_output(&mut self, output: ClientOutboundOutput, info: ClientOutboundInfo) {
        match (output, info) {
            (ClientOutboundOutput::Pricing, ClientOutboundInfo::Pricing) => {
                self.announce_in_flight = false;
                self.requeue_superseded_announce();
                if let Some(overlay) = self.overlay() {
                    self.pending_events.push_back(HandlerEvent::Peer {
                        overlay,
                        event: PeerEvent::PaymentThresholdSent,
                    });
                }
            }
            (
                ClientOutboundOutput::Retrieval(delivery),
                ClientOutboundInfo::Retrieval {
                    address,
                    response,
                    requested_at,
                    originated,
                },
            ) => {
                let latency = requested_at.elapsed();
                self.on_retrieval_response(delivery, address, response, latency, originated);
            }
            (
                ClientOutboundOutput::Pushsync(receipt),
                ClientOutboundInfo::Pushsync {
                    address,
                    response,
                    requested_at,
                    originated,
                },
            ) => {
                let latency = requested_at.elapsed();
                debug!(%address, "Received pushsync receipt");
                self.on_pushsync_receipt(receipt, address, response, latency, originated);
            }
            (
                ClientOutboundOutput::Pseudosettle(ack),
                ClientOutboundInfo::Pseudosettle { amount },
            ) => {
                if let Some(overlay) = self.overlay() {
                    if ack.amount != amount {
                        warn!(
                            %overlay,
                            sent = %amount,
                            acked = %ack.amount,
                            "Pseudosettle ack amount mismatch"
                        );
                    }
                    debug!(%overlay, %amount, ack_amount = %ack.amount, "Pseudosettle sent");
                    let ack = domain_ack(ack);
                    self.pending_events.push_back(HandlerEvent::Peer {
                        overlay,
                        event: PeerEvent::PseudosettleSent { ack },
                    });
                }
            }
            #[cfg(feature = "swap")]
            (ClientOutboundOutput::Swap(headers), ClientOutboundInfo::Swap) => {
                if let Some(overlay) = self.overlay() {
                    debug!(%overlay, peer_rate = %headers.exchange_rate, "Swap cheque sent");
                    self.push_event(HandlerEvent::Peer {
                        overlay,
                        event: PeerEvent::SwapChequeSent {
                            peer_rate: headers.exchange_rate,
                        },
                    });
                }
            }
            (output, info) => {
                warn!(?output, ?info, "Mismatched outbound output and info");
            }
        }
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
    PseudosettleAck {
        accepted: Au::saturating_from_u256(ack.amount),
        timestamp: ack.timestamp,
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{B256, Signature};
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk};
    use vertex_swarm_primitives::{StampedChunk, StampedChunkExt};

    fn stamped(payload: &'static [u8]) -> StampedChunk {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
        let chunk: AnyChunk = ContentChunk::new(payload)
            .expect("valid content chunk")
            .into();
        StampedChunk::new(chunk, stamp)
    }

    struct NoopStore;

    impl vertex_swarm_api::SwarmLocalStore for NoopStore {
        fn put(
            &self,
            _chunk: vertex_swarm_primitives::CachedChunk,
        ) -> vertex_swarm_api::SwarmResult<()> {
            Ok(())
        }
        fn get(
            &self,
            _address: &nectar_primitives::ChunkAddress,
        ) -> vertex_swarm_api::SwarmResult<Option<vertex_swarm_primitives::CachedChunk>> {
            Ok(None)
        }
        fn contains(&self, _address: &nectar_primitives::ChunkAddress) -> bool {
            false
        }
        fn remove(
            &self,
            _address: &nectar_primitives::ChunkAddress,
        ) -> vertex_swarm_api::SwarmResult<()> {
            Ok(())
        }
    }

    #[test]
    fn payment_threshold_before_activation_is_buffered_and_flushed() {
        use alloy_primitives::U256;
        use vertex_swarm_client_protocol::PeerEvent;
        use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

        use super::{ClientHandler, Config, HandlerEvent};
        use crate::forward::StubForwarder;

        let mut handler = ClientHandler::new(
            Config::default(),
            std::sync::Arc::new(NoopStore),
            std::sync::Arc::new(StubForwarder),
            None,
        );

        // A payment threshold arriving while dormant is buffered, not dropped.
        handler.on_payment_threshold_received(
            vertex_swarm_net_pricing::AnnouncePaymentThreshold::new(U256::from(9_000_000u64)),
        );
        assert_eq!(
            handler.pending_peer_threshold,
            Some(U256::from(9_000_000u64))
        );

        // Activation emits Activated first, then flushes the buffered payment threshold.
        let overlay = OverlayAddress::from([1u8; 32]);
        handler.activate(overlay, SwarmNodeType::Storer);
        assert_eq!(handler.pending_peer_threshold, None);

        assert!(matches!(
            handler.pending_events.pop(),
            Some(HandlerEvent::Activated { overlay: o }) if o == overlay
        ));
        assert!(matches!(
            handler.pending_events.pop(),
            Some(HandlerEvent::Peer {
                event: PeerEvent::PaymentThresholdReceived { threshold },
                ..
            }) if threshold == U256::from(9_000_000u64)
        ));
    }

    /// Drive the handler until it requests an outbound substream, ignoring
    /// behaviour notifications on the way. Returns whether a substream was
    /// requested before the handler went idle.
    fn polls_a_substream_request(handler: &mut super::ClientHandler) -> bool {
        use libp2p::swarm::{ConnectionHandler, ConnectionHandlerEvent};
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        loop {
            match handler.poll(&mut cx) {
                std::task::Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                    ..
                }) => return true,
                std::task::Poll::Ready(_) => continue,
                std::task::Poll::Pending => return false,
            }
        }
    }

    fn announce(handler: &mut super::ClientHandler, threshold: u64) {
        use alloy_primitives::U256;
        use libp2p::swarm::ConnectionHandler;
        use vertex_swarm_client_protocol::PeerCommand;
        handler.on_behaviour_event(super::HandlerCommand::Peer(
            PeerCommand::AnnouncePaymentThreshold {
                threshold: U256::from(threshold),
            },
        ));
    }

    fn active_handler() -> super::ClientHandler {
        use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};
        let mut handler = super::ClientHandler::new(
            super::Config::default(),
            std::sync::Arc::new(NoopStore),
            std::sync::Arc::new(crate::forward::StubForwarder),
            None,
        );
        handler.activate(OverlayAddress::from([1u8; 32]), SwarmNodeType::Storer);
        handler
    }

    #[test]
    fn re_announcement_opens_a_fresh_pricing_substream() {
        use crate::upgrade::{ClientOutboundInfo, ClientOutboundOutput};

        let mut handler = active_handler();

        // Initial announcement goes out.
        announce(&mut handler, 13_500_000);
        assert!(polls_a_substream_request(&mut handler));

        // The substream resolves; a checkpoint re-announcement then opens a
        // fresh substream instead of being latched away.
        handler.handle_outbound_output(ClientOutboundOutput::Pricing, ClientOutboundInfo::Pricing);
        announce(&mut handler, 18_000_000);
        assert!(polls_a_substream_request(&mut handler));
    }

    #[test]
    fn announcement_superseded_in_flight_is_sent_on_completion() {
        use crate::upgrade::{ClientOutboundInfo, ClientOutboundOutput};
        use alloy_primitives::U256;

        let mut handler = active_handler();

        // One announcement in flight; a second arriving meanwhile is buffered,
        // not sent concurrently and not dropped.
        announce(&mut handler, 13_500_000);
        assert!(polls_a_substream_request(&mut handler));
        announce(&mut handler, 18_000_000);
        assert!(!polls_a_substream_request(&mut handler));
        assert_eq!(handler.superseded_announce, Some(U256::from(18_000_000u64)));

        // Completion re-enqueues the buffered newest line.
        handler.handle_outbound_output(ClientOutboundOutput::Pricing, ClientOutboundInfo::Pricing);
        assert_eq!(handler.superseded_announce, None);
        assert!(polls_a_substream_request(&mut handler));
    }

    #[test]
    fn verify_answers_gate_accepts_matching_chunk() {
        let chunk = stamped(b"serve gate payload");
        let requested = *chunk.address();
        assert!(chunk.verify_answers(requested).is_ok());
    }

    #[test]
    fn verify_answers_gate_rejects_mismatched_chunk() {
        let chunk = stamped(b"actual payload");
        let other = stamped(b"a different payload entirely");
        let requested = *other.address();
        assert_ne!(*chunk.address(), requested);
        assert!(chunk.verify_answers(requested).is_err());
    }

    #[test]
    fn ack_round_trip_saturates_amount_and_passes_timestamp() {
        use alloy_primitives::U256;
        use vertex_swarm_api::Au;
        use vertex_swarm_client_protocol::PseudosettleAck;
        use vertex_swarm_net_pseudosettle::PaymentAck;

        use super::{domain_ack, wire_ack};

        // An in-spec amount survives domain -> wire -> domain unchanged, and the
        // sampled timestamp passes through verbatim.
        let ack = PseudosettleAck {
            accepted: Au::from_amount(4200),
            timestamp: 987_654,
        };
        let round = domain_ack(wire_ack(ack));
        assert_eq!(round.accepted, ack.accepted);
        assert_eq!(round.timestamp, ack.timestamp);

        // A wire amount beyond the u64 AU range saturates in AU space rather than
        // wrapping to a small value; the timestamp still passes through.
        let out_of_spec = PaymentAck::new(U256::MAX, -1);
        let dom = domain_ack(out_of_spec);
        assert_eq!(dom.accepted, Au::saturating_from_u256(U256::MAX));
        assert_eq!(dom.timestamp, -1);
    }
}
