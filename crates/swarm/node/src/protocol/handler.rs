//! Connection handler for client protocols.
//!
//! The `ClientHandler` manages multiple protocols on a single connection:
//! - Pricing: Payment threshold exchange
//! - Retrieval: Chunk request/response
//! - PushSync: Chunk push with receipt
//! - Pseudosettle: Bandwidth accounting payments
//!
//! # Lifecycle
//!
//! 1. Handler starts in `Dormant` state when connection established
//! 2. After handshake, `Activate` command transitions to `Active` state
//! 3. In `Active` state, handler processes protocol messages
//!
//! # Responder Storage
//!
//! Inbound requests (retrieval, pushsync, pseudosettle) arrive with responders
//! that must be stored until the application layer provides a response. The handler
//! maps request IDs to [`PendingResponse`] entries, bounded per connection.

use std::{
    collections::{HashMap, VecDeque},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use alloy_primitives::U256;
use futures_bounded::Timeout;
use libp2p::swarm::{
    SubstreamProtocol,
    handler::{
        ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
        FullyNegotiatedOutbound,
    },
};
use nectar_primitives::ChunkAddress;
use tracing::{debug, warn};
use vertex_swarm_net_pseudosettle::PaymentAck;
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

use super::upgrade::{
    ClientInboundOutput, ClientInboundUpgrade, ClientOutboundInfo, ClientOutboundOutput,
    ClientOutboundUpgrade,
};

const DEFAULT_MAX_PENDING_COMMANDS: usize = 256;
const DEFAULT_MAX_PENDING_EVENTS: usize = 256;
/// Maximum number of stored responders per connection.
const MAX_PENDING_RESPONSES: usize = 64;
/// Timeout for async response sending (prevent stuck streams).
const RESPONSE_SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum concurrent response sends per connection.
const MAX_CONCURRENT_RESPONSE_SENDS: usize = 8;
/// Responders older than this are dropped as stale.
const RESPONDER_STALE_TIMEOUT: Duration = Duration::from_secs(60);

/// Configuration for the client handler.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Timeout for protocol operations.
    pub(crate) timeout: Duration,
    /// Maximum pending commands before dropping new ones.
    pub(crate) max_pending_commands: usize,
    /// Maximum pending events before dropping new ones.
    pub(crate) max_pending_events: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_pending_commands: DEFAULT_MAX_PENDING_COMMANDS,
            max_pending_events: DEFAULT_MAX_PENDING_EVENTS,
        }
    }
}

/// Commands sent from the behaviour to the handler.
#[derive(Debug)]
pub(crate) enum HandlerCommand {
    /// Activate the handler after handshake completion.
    Activate {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The peer's node type.
        node_type: SwarmNodeType,
    },
    /// Announce our payment threshold to the peer.
    AnnouncePricing {
        /// The threshold to announce.
        threshold: U256,
    },
    /// Request a chunk from the peer.
    RetrieveChunk {
        /// The address of the chunk to retrieve.
        address: ChunkAddress,
    },
    /// Push a chunk to the peer for storage.
    PushChunk {
        /// The chunk address.
        address: ChunkAddress,
        /// The chunk data.
        data: bytes::Bytes,
        /// The postage stamp.
        stamp: bytes::Bytes,
    },
    /// Send a pseudosettle payment to the peer.
    SendPseudosettle {
        /// The amount to send.
        amount: U256,
    },
    /// Acknowledge a pseudosettle payment.
    AckPseudosettle {
        /// Request ID to match the responder.
        request_id: u64,
        /// The ack to send.
        ack: PaymentAck,
    },
    /// Serve a chunk to a peer (respond to retrieval request).
    ServeChunk {
        /// Request ID from the ChunkRequested event.
        request_id: u64,
        /// The chunk data.
        data: bytes::Bytes,
        /// The postage stamp.
        stamp: bytes::Bytes,
    },
    /// Send a receipt to a peer (respond to pushsync delivery).
    SendReceipt {
        /// Request ID from the ChunkPushReceived event.
        request_id: u64,
        /// The chunk address.
        address: ChunkAddress,
        /// The receipt signature.
        signature: bytes::Bytes,
        /// The receipt nonce.
        nonce: bytes::Bytes,
        /// Our storage radius.
        storage_radius: u8,
    },
}

/// Events emitted by the handler to the behaviour.
#[derive(Debug)]
pub(crate) enum HandlerEvent {
    /// Handler has been activated.
    Activated {
        /// The peer's overlay address.
        overlay: OverlayAddress,
    },
    /// Received pricing threshold from peer.
    PricingReceived {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The payment threshold.
        threshold: U256,
    },
    /// Successfully sent our pricing threshold.
    PricingSent {
        /// The peer's overlay address.
        overlay: OverlayAddress,
    },
    /// Received a chunk request from peer.
    ChunkRequested {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The requested chunk address.
        address: ChunkAddress,
        /// Request ID for correlating response.
        request_id: u64,
    },
    /// Received a chunk from peer.
    ChunkReceived {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// The chunk data.
        data: bytes::Bytes,
        /// The postage stamp.
        stamp: bytes::Bytes,
    },
    /// Received a chunk push from peer.
    ChunkPushReceived {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// The chunk data.
        data: bytes::Bytes,
        /// The postage stamp.
        stamp: bytes::Bytes,
        /// Request ID for correlating receipt.
        request_id: u64,
    },
    /// Received a receipt from peer.
    ReceiptReceived {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// The storer's signature.
        signature: bytes::Bytes,
        /// The nonce used.
        nonce: bytes::Bytes,
        /// The storer's storage radius.
        storage_radius: u8,
    },
    /// Protocol error occurred.
    Error {
        /// The peer's overlay address (if known).
        overlay: Option<OverlayAddress>,
        /// The protocol that errored.
        protocol: &'static str,
        /// Error description.
        error: String,
    },
    /// Received pseudosettle payment from peer.
    PseudosettleReceived {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The payment amount.
        amount: U256,
        /// Request ID for matching ack.
        request_id: u64,
    },
    /// Successfully sent pseudosettle payment.
    PseudosettleSent {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The ack received.
        ack: PaymentAck,
    },
}

/// Handler state machine.
#[derive(Debug)]
enum State {
    /// Waiting for activation command.
    Dormant,
    /// Active and processing protocols.
    Active { overlay: OverlayAddress },
}

/// A pending inbound response waiting for the application layer to provide data.
enum PendingResponse {
    /// Awaiting chunk data to serve (retrieval response).
    Retrieval(vertex_swarm_net_retrieval::RetrievalResponder),
    /// Awaiting receipt to send (pushsync response).
    Pushsync(vertex_swarm_net_pushsync::PushsyncResponder),
    /// Awaiting ack to send (pseudosettle response).
    Pseudosettle(vertex_swarm_net_pseudosettle::PseudosettleInboundResult),
}

/// Metadata for tracking when a responder was stored.
struct StoredResponse {
    response: PendingResponse,
    stored_at: Instant,
}

/// Swarm client connection handler.
///
/// Manages multiple client protocols on a single peer connection.
pub(crate) struct ClientHandler {
    config: Config,
    state: State,
    /// Counter for request IDs.
    next_request_id: u64,
    /// Pending commands to process.
    pending_commands: VecDeque<HandlerCommand>,
    /// Pending events to emit.
    pending_events: VecDeque<HandlerEvent>,
    /// Whether pricing has been sent.
    pricing_sent: bool,
    /// Whether pricing outbound is pending.
    pricing_outbound_pending: bool,
    /// Stored responders waiting for application-layer responses, keyed by request_id.
    pending_responses: HashMap<u64, StoredResponse>,
    /// Bounded set for async response sends (prevents blocking poll).
    response_sends: futures_bounded::FuturesSet<Result<(), String>>,
}

impl ClientHandler {
    /// Push an event if the queue isn't full, otherwise drop with a metric.
    fn push_event(&mut self, event: HandlerEvent) {
        if self.pending_events.len() >= self.config.max_pending_events {
            warn!("Handler event queue full, dropping event");
            metrics::counter!("swarm.client.handler.events_dropped").increment(1);
            return;
        }
        self.pending_events.push_back(event);
    }

    /// Create a new handler in dormant state.
    pub(crate) fn new(config: Config) -> Self {
        Self {
            config,
            state: State::Dormant,
            next_request_id: 0,
            pending_commands: VecDeque::new(),
            pending_events: VecDeque::new(),
            pricing_sent: false,
            pricing_outbound_pending: false,
            pending_responses: HashMap::new(),
            response_sends: futures_bounded::FuturesSet::new(
                RESPONSE_SEND_TIMEOUT,
                MAX_CONCURRENT_RESPONSE_SENDS,
            ),
        }
    }

    /// Get the overlay address if active.
    fn overlay(&self) -> Option<OverlayAddress> {
        match &self.state {
            State::Active { overlay, .. } => Some(*overlay),
            _ => None,
        }
    }

    /// Generate the next request ID.
    fn next_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        id
    }

    /// Store a pending response, evicting stale entries if at capacity.
    fn store_response(&mut self, request_id: u64, response: PendingResponse) {
        if self.pending_responses.len() >= MAX_PENDING_RESPONSES {
            self.evict_stale_responses();
        }
        if self.pending_responses.len() >= MAX_PENDING_RESPONSES {
            warn!(%request_id, "Pending response map full, dropping oldest");
            metrics::counter!("swarm.client.handler.responses_dropped").increment(1);
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

    /// Remove responders older than the stale timeout.
    fn evict_stale_responses(&mut self) {
        let cutoff = Instant::now() - RESPONDER_STALE_TIMEOUT;
        self.pending_responses.retain(|_, v| v.stored_at > cutoff);
    }

    /// Take a pending response by request ID.
    fn take_response(&mut self, request_id: u64) -> Option<PendingResponse> {
        self.pending_responses
            .remove(&request_id)
            .map(|s| s.response)
    }

    /// Process activation command.
    fn activate(&mut self, overlay: OverlayAddress, node_type: SwarmNodeType) {
        match &self.state {
            State::Dormant => {
                debug!(%overlay, ?node_type, "Handler activated");
                self.state = State::Active { overlay };
                self.pending_events
                    .push_back(HandlerEvent::Activated { overlay });
            }
            State::Active { .. } => {
                warn!("Handler already active, ignoring duplicate activation");
            }
        }
    }

    /// Handle incoming pricing threshold.
    fn on_pricing_received(
        &mut self,
        threshold: vertex_swarm_net_pricing::AnnouncePaymentThreshold,
    ) {
        if let Some(overlay) = self.overlay() {
            debug!(%overlay, threshold = %threshold.payment_threshold, "Received pricing");
            self.pending_events
                .push_back(HandlerEvent::PricingReceived {
                    overlay,
                    threshold: threshold.payment_threshold,
                });
        } else {
            warn!(
                threshold = %threshold.payment_threshold,
                "Received pricing in dormant state (peer may have cached old protocol list)"
            );
        }
    }

    /// Handle incoming retrieval request.
    fn on_retrieval_request(
        &mut self,
        request: vertex_swarm_net_retrieval::Request,
        responder: vertex_swarm_net_retrieval::RetrievalResponder,
    ) {
        if let Some(overlay) = self.overlay() {
            let request_id = self.next_request_id();
            debug!(%overlay, address = %request.address, %request_id, "Received retrieval request");
            self.push_event(HandlerEvent::ChunkRequested {
                overlay,
                address: request.address,
                request_id,
            });
            self.store_response(request_id, PendingResponse::Retrieval(responder));
        } else {
            warn!(
                address = %request.address,
                "Received retrieval request in dormant state (peer may have cached old protocol list)"
            );
        }
    }

    /// Handle incoming pushsync delivery.
    fn on_pushsync_delivery(
        &mut self,
        delivery: vertex_swarm_net_pushsync::Delivery,
        responder: vertex_swarm_net_pushsync::PushsyncResponder,
    ) {
        if let Some(overlay) = self.overlay() {
            let request_id = self.next_request_id();
            debug!(%overlay, address = %delivery.address, %request_id, "Received pushsync delivery");
            self.pending_events
                .push_back(HandlerEvent::ChunkPushReceived {
                    overlay,
                    address: delivery.address,
                    data: delivery.data,
                    stamp: delivery.stamp,
                    request_id,
                });
            self.store_response(request_id, PendingResponse::Pushsync(responder));
        } else {
            warn!(
                address = %delivery.address,
                "Received pushsync delivery in dormant state (peer may have cached old protocol list)"
            );
        }
    }

    /// Handle retrieval response.
    fn on_retrieval_response(
        &mut self,
        delivery: vertex_swarm_net_retrieval::Delivery,
        address: ChunkAddress,
    ) {
        if let Some(overlay) = self.overlay() {
            if let Some(ref err) = delivery.error {
                debug!(%overlay, error = %err, "Retrieval failed");
                self.push_event(HandlerEvent::Error {
                    overlay: Some(overlay),
                    protocol: "retrieval",
                    error: err.clone(),
                });
            } else {
                debug!(%overlay, data_len = delivery.data.len(), "Received chunk");
                self.push_event(HandlerEvent::ChunkReceived {
                    overlay,
                    address,
                    data: delivery.data,
                    stamp: delivery.stamp,
                });
            }
        }
    }

    /// Handle pushsync receipt.
    fn on_pushsync_receipt(&mut self, receipt: vertex_swarm_net_pushsync::Receipt) {
        if let Some(overlay) = self.overlay() {
            if let Some(ref err) = receipt.error {
                debug!(%overlay, error = %err, "Pushsync failed");
                self.push_event(HandlerEvent::Error {
                    overlay: Some(overlay),
                    protocol: "pushsync",
                    error: err.clone(),
                });
            } else {
                debug!(%overlay, address = %receipt.address, "Received receipt");
                self.pending_events
                    .push_back(HandlerEvent::ReceiptReceived {
                        overlay,
                        address: receipt.address,
                        signature: receipt.signature,
                        nonce: receipt.nonce,
                        storage_radius: receipt.storage_radius,
                    });
            }
        }
    }
}

/// Multi-protocol ConnectionHandler implementation.
#[allow(deprecated)]
impl ConnectionHandler for ClientHandler {
    type FromBehaviour = HandlerCommand;
    type ToBehaviour = HandlerEvent;
    type InboundProtocol = ClientInboundUpgrade;
    type OutboundProtocol = ClientOutboundUpgrade;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ClientOutboundInfo;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        let upgrade = match &self.state {
            State::Active { .. } => ClientInboundUpgrade::active(),
            State::Dormant => ClientInboundUpgrade::new(),
        };
        SubstreamProtocol::new(upgrade, ()).with_timeout(self.config.timeout)
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        // Emit pending events first
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Drain completed response sends
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
            if let Some(event) = self.pending_events.pop_front() {
                return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
            }
        }

        // Process pending commands
        while let Some(cmd) = self.pending_commands.pop_front() {
            match cmd {
                HandlerCommand::Activate { overlay, node_type } => {
                    self.activate(overlay, node_type);
                    if let Some(event) = self.pending_events.pop_front() {
                        return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
                    }
                }
                HandlerCommand::AnnouncePricing { threshold } => {
                    if !self.pricing_sent && !self.pricing_outbound_pending {
                        self.pricing_outbound_pending = true;
                        let announce =
                            vertex_swarm_net_pricing::AnnouncePaymentThreshold::new(threshold);
                        let upgrade = ClientOutboundUpgrade::pricing(announce);
                        return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                            protocol: SubstreamProtocol::new(upgrade, ClientOutboundInfo::Pricing)
                                .with_timeout(self.config.timeout),
                        });
                    }
                }
                HandlerCommand::RetrieveChunk { address } => {
                    let upgrade = ClientOutboundUpgrade::retrieval(address);
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(
                            upgrade,
                            ClientOutboundInfo::Retrieval { address },
                        )
                        .with_timeout(self.config.timeout),
                    });
                }
                HandlerCommand::PushChunk {
                    address,
                    data,
                    stamp,
                } => {
                    let delivery = vertex_swarm_net_pushsync::Delivery::new(address, data, stamp);
                    let upgrade = ClientOutboundUpgrade::pushsync(delivery);
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(
                            upgrade,
                            ClientOutboundInfo::Pushsync { address },
                        )
                        .with_timeout(self.config.timeout),
                    });
                }
                HandlerCommand::SendPseudosettle { amount } => {
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
                HandlerCommand::AckPseudosettle { request_id, ack } => {
                    if let Some(PendingResponse::Pseudosettle(result)) =
                        self.take_response(request_id)
                    {
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
                HandlerCommand::ServeChunk {
                    request_id,
                    data,
                    stamp,
                } => {
                    if let Some(PendingResponse::Retrieval(responder)) =
                        self.take_response(request_id)
                    {
                        debug!(%request_id, "Serving chunk");
                        if self
                            .response_sends
                            .try_push(async move {
                                responder
                                    .send_chunk(data, stamp)
                                    .await
                                    .map_err(|e| format!("serve chunk: {e}"))
                            })
                            .is_err()
                        {
                            warn!("Response send queue full, dropping chunk serve");
                        }
                    } else {
                        warn!(%request_id, "No retrieval responder found for request_id");
                    }
                }
                HandlerCommand::SendReceipt {
                    request_id,
                    address,
                    signature,
                    nonce,
                    storage_radius,
                } => {
                    if let Some(PendingResponse::Pushsync(responder)) =
                        self.take_response(request_id)
                    {
                        debug!(%request_id, %address, "Sending receipt");
                        let receipt = vertex_swarm_net_pushsync::Receipt::success(
                            address,
                            signature,
                            nonce,
                            storage_radius,
                        );
                        if self
                            .response_sends
                            .try_push(async move {
                                responder
                                    .send_receipt(receipt)
                                    .await
                                    .map_err(|e| format!("send receipt: {e}"))
                            })
                            .is_err()
                        {
                            warn!("Response send queue full, dropping receipt send");
                        }
                    } else {
                        warn!(%request_id, "No pushsync responder found for request_id");
                    }
                }
            }
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        if self.pending_commands.len() >= self.config.max_pending_commands {
            warn!("Handler command queue full, dropping command");
            metrics::counter!("swarm.client.handler.commands_dropped").increment(1);
            return;
        }
        self.pending_commands.push_back(event);
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
                let protocol = match &e.info {
                    ClientOutboundInfo::Pricing => {
                        self.pricing_outbound_pending = false;
                        "pricing"
                    }
                    ClientOutboundInfo::Retrieval { .. } => "retrieval",
                    ClientOutboundInfo::Pushsync { .. } => "pushsync",
                    ClientOutboundInfo::Pseudosettle { .. } => "pseudosettle",
                };
                warn!(protocol, error = %e.error, "Client dial upgrade error");
                self.push_event(HandlerEvent::Error {
                    overlay: self.overlay(),
                    protocol,
                    error: e.error.to_string(),
                });
            }

            ConnectionEvent::ListenUpgradeError(e) => {
                warn!(error = %e.error, "Client listen upgrade error");
                self.push_event(HandlerEvent::Error {
                    overlay: self.overlay(),
                    protocol: "unknown",
                    error: e.error.to_string(),
                });
            }

            _ => {}
        }
    }
}

impl ClientHandler {
    /// Handle an inbound protocol completion.
    fn handle_inbound_output(&mut self, output: ClientInboundOutput) {
        match output {
            ClientInboundOutput::Pricing(threshold) => {
                self.on_pricing_received(threshold);
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
                    self.pending_events
                        .push_back(HandlerEvent::PseudosettleReceived {
                            overlay,
                            amount: result.payment.amount,
                            request_id,
                        });
                    self.store_response(request_id, PendingResponse::Pseudosettle(result));
                }
            }
        }
    }

    /// Handle an outbound protocol completion.
    fn handle_outbound_output(&mut self, output: ClientOutboundOutput, info: ClientOutboundInfo) {
        match (output, info) {
            (ClientOutboundOutput::Pricing, ClientOutboundInfo::Pricing) => {
                self.pricing_sent = true;
                self.pricing_outbound_pending = false;
                if let Some(overlay) = self.overlay() {
                    self.pending_events
                        .push_back(HandlerEvent::PricingSent { overlay });
                }
            }
            (
                ClientOutboundOutput::Retrieval(delivery),
                ClientOutboundInfo::Retrieval { address },
            ) => {
                self.on_retrieval_response(delivery, address);
            }
            (ClientOutboundOutput::Pushsync(receipt), ClientOutboundInfo::Pushsync { address }) => {
                if let Some(overlay) = self.overlay() {
                    debug!(%overlay, %address, "Received pushsync receipt");
                    self.on_pushsync_receipt(receipt);
                }
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
                    self.pending_events
                        .push_back(HandlerEvent::PseudosettleSent { overlay, ack });
                }
            }
            (output, info) => {
                warn!(?output, ?info, "Mismatched outbound output and info");
            }
        }
    }
}
