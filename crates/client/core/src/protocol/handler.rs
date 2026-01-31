//! Connection handler for client protocols.
//!
//! The `SwarmClientHandler` manages multiple protocols on a single connection:
//! - Pricing: Payment threshold exchange
//! - Retrieval: Chunk request/response
//! - PushSync: Chunk push with receipt
//!
//! # Lifecycle
//!
//! 1. Handler starts in `Dormant` state when connection established
//! 2. After handshake, `Activate` command transitions to `Active` state
//! 3. In `Active` state, handler processes protocol messages
//!
//! # Multi-Protocol Support
//!
//! The handler uses `ClientInboundUpgrade` which advertises all three
//! protocols (pricing, retrieval, pushsync) and dispatches based on the
//! negotiated protocol.

use std::{
    collections::VecDeque,
    task::{Context, Poll},
    time::Duration,
};

use alloy_primitives::U256;
use libp2p::swarm::{
    SubstreamProtocol,
    handler::{
        ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
        FullyNegotiatedOutbound,
    },
};
use tracing::{debug, warn};
use vertex_bandwidth_chequebook::SignedCheque;
use vertex_net_pseudosettle::PaymentAck;
use vertex_primitives::{ChunkAddress, OverlayAddress};

use super::upgrade::{
    ClientInboundOutput, ClientInboundUpgrade, ClientOutboundInfo, ClientOutboundOutput,
    ClientOutboundUpgrade,
};

/// Configuration for the client handler.
#[derive(Debug, Clone)]
pub struct Config {
    /// Timeout for protocol operations.
    pub timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

/// Commands sent from the behaviour to the handler.
#[derive(Debug)]
pub enum HandlerCommand {
    /// Activate the handler after handshake completion.
    Activate {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// Whether the peer is a full node.
        is_full_node: bool,
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
    /// Send a swap cheque to the peer.
    SendCheque {
        /// The signed cheque.
        cheque: SignedCheque,
        /// Our exchange rate.
        our_rate: U256,
    },
    /// Acknowledge a pseudosettle payment.
    AckPseudosettle {
        /// Request ID to match the responder.
        request_id: u64,
        /// The ack to send.
        ack: PaymentAck,
    },
}

/// Events emitted by the handler to the behaviour.
#[derive(Debug)]
pub enum HandlerEvent {
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
    /// Received swap cheque from peer.
    ChequeReceived {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The signed cheque.
        cheque: SignedCheque,
        /// The peer's exchange rate.
        peer_rate: U256,
    },
    /// Successfully sent swap cheque.
    ChequeSent {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The peer's exchange rate.
        peer_rate: U256,
    },
}

/// Handler state machine.
#[derive(Debug)]
enum State {
    /// Waiting for activation command.
    Dormant,
    /// Active and processing protocols.
    Active {
        overlay: OverlayAddress,
        is_full_node: bool,
    },
    /// Handler is closing.
    Closing,
}

/// Outbound protocol selection.
#[derive(Debug, Clone)]
pub enum OutboundProtocol {
    Pricing(vertex_net_pricing::PricingOutboundProtocol),
    Retrieval(vertex_net_retrieval::RetrievalOutboundProtocol),
    PushSync(vertex_net_pushsync::PushsyncOutboundProtocol),
}

/// Inbound protocol selection.
#[derive(Debug, Clone)]
pub enum InboundProtocol {
    Pricing(vertex_net_pricing::PricingInboundProtocol),
    Retrieval(vertex_net_retrieval::RetrievalInboundProtocol),
    PushSync(vertex_net_pushsync::PushsyncInboundProtocol),
}

/// Inbound protocol output after negotiation.
pub enum InboundOutput {
    Pricing(vertex_net_pricing::AnnouncePaymentThreshold),
    Retrieval(
        (
            vertex_net_retrieval::Request,
            vertex_net_retrieval::RetrievalResponder,
        ),
    ),
    PushSync(
        (
            vertex_net_pushsync::Delivery,
            vertex_net_pushsync::PushsyncResponder,
        ),
    ),
}

/// Outbound protocol output after negotiation.
pub enum OutboundOutput {
    Pricing,
    Retrieval(vertex_net_retrieval::Delivery),
    PushSync(vertex_net_pushsync::Receipt),
}

/// Swarm client connection handler.
///
/// Manages multiple client protocols on a single peer connection.
pub struct SwarmClientHandler {
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
}

impl SwarmClientHandler {
    /// Create a new handler in dormant state.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            state: State::Dormant,
            next_request_id: 0,
            pending_commands: VecDeque::new(),
            pending_events: VecDeque::new(),
            pricing_sent: false,
            pricing_outbound_pending: false,
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

    /// Process activation command.
    fn activate(&mut self, overlay: OverlayAddress, is_full_node: bool) {
        match &self.state {
            State::Dormant => {
                debug!(%overlay, %is_full_node, "Handler activated");
                self.state = State::Active {
                    overlay,
                    is_full_node,
                };
                self.pending_events
                    .push_back(HandlerEvent::Activated { overlay });
            }
            State::Active { .. } => {
                warn!("Handler already active, ignoring duplicate activation");
            }
            State::Closing => {
                warn!("Handler closing, ignoring activation");
            }
        }
    }

    /// Handle incoming pricing threshold.
    fn on_pricing_received(&mut self, threshold: vertex_net_pricing::AnnouncePaymentThreshold) {
        if let Some(overlay) = self.overlay() {
            debug!(%overlay, threshold = %threshold.payment_threshold, "Received pricing");
            self.pending_events
                .push_back(HandlerEvent::PricingReceived {
                    overlay,
                    threshold: threshold.payment_threshold,
                });
        } else {
            warn!("Received pricing in dormant state");
        }
    }

    /// Handle incoming retrieval request.
    fn on_retrieval_request(
        &mut self,
        request: vertex_net_retrieval::Request,
        _responder: vertex_net_retrieval::RetrievalResponder,
    ) {
        if let Some(overlay) = self.overlay() {
            let request_id = self.next_request_id();
            debug!(%overlay, address = %request.address, %request_id, "Received retrieval request");
            self.pending_events.push_back(HandlerEvent::ChunkRequested {
                overlay,
                address: request.address,
                request_id,
            });
            // TODO: Store responder for later use
        } else {
            warn!("Received retrieval request in dormant state");
        }
    }

    /// Handle incoming pushsync delivery.
    fn on_pushsync_delivery(
        &mut self,
        delivery: vertex_net_pushsync::Delivery,
        _responder: vertex_net_pushsync::PushsyncResponder,
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
            // TODO: Store responder for later use
        } else {
            warn!("Received pushsync delivery in dormant state");
        }
    }

    /// Handle retrieval response.
    fn on_retrieval_response(
        &mut self,
        delivery: vertex_net_retrieval::Delivery,
        address: ChunkAddress,
    ) {
        if let Some(overlay) = self.overlay() {
            if let Some(ref err) = delivery.error {
                debug!(%overlay, error = %err, "Retrieval failed");
                self.pending_events.push_back(HandlerEvent::Error {
                    overlay: Some(overlay),
                    protocol: "retrieval",
                    error: err.clone(),
                });
            } else {
                debug!(%overlay, data_len = delivery.data.len(), "Received chunk");
                self.pending_events.push_back(HandlerEvent::ChunkReceived {
                    overlay,
                    address,
                    data: delivery.data,
                    stamp: delivery.stamp,
                });
            }
        }
    }

    /// Handle pushsync receipt.
    fn on_pushsync_receipt(&mut self, receipt: vertex_net_pushsync::Receipt) {
        if let Some(overlay) = self.overlay() {
            if let Some(ref err) = receipt.error {
                debug!(%overlay, error = %err, "Pushsync failed");
                self.pending_events.push_back(HandlerEvent::Error {
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
///
/// Uses `ClientInboundUpgrade` to advertise pricing, retrieval, and pushsync protocols.
/// Uses `ClientOutboundUpgrade` for outbound requests with `ClientOutboundInfo`
/// to track which request type is in flight.
impl ConnectionHandler for SwarmClientHandler {
    type FromBehaviour = HandlerCommand;
    type ToBehaviour = HandlerEvent;
    type InboundProtocol = ClientInboundUpgrade;
    type OutboundProtocol = ClientOutboundUpgrade;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ClientOutboundInfo;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(ClientInboundUpgrade::new(), ()).with_timeout(self.config.timeout)
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        // Emit pending events first
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Process pending commands
        while let Some(cmd) = self.pending_commands.pop_front() {
            match cmd {
                HandlerCommand::Activate {
                    overlay,
                    is_full_node,
                } => {
                    self.activate(overlay, is_full_node);
                    if let Some(event) = self.pending_events.pop_front() {
                        return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
                    }
                }
                HandlerCommand::AnnouncePricing { threshold } => {
                    if !self.pricing_sent && !self.pricing_outbound_pending {
                        self.pricing_outbound_pending = true;
                        let announce = vertex_net_pricing::AnnouncePaymentThreshold::new(threshold);
                        let upgrade = ClientOutboundUpgrade::pricing(announce);
                        return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                            protocol: SubstreamProtocol::new(upgrade, ClientOutboundInfo::Pricing)
                                .with_timeout(self.config.timeout),
                        });
                    }
                }
                HandlerCommand::RetrieveChunk { address } => {
                    // Create retrieval outbound request
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
                    // Create pushsync outbound request
                    let delivery = vertex_net_pushsync::Delivery::new(address, data, stamp);
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
                    let payment = vertex_net_pseudosettle::Payment::new(amount);
                    let upgrade = ClientOutboundUpgrade::pseudosettle(payment);
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(
                            upgrade,
                            ClientOutboundInfo::Pseudosettle { amount },
                        )
                        .with_timeout(self.config.timeout),
                    });
                }
                HandlerCommand::SendCheque { cheque, our_rate } => {
                    let upgrade = ClientOutboundUpgrade::swap(cheque, our_rate);
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(upgrade, ClientOutboundInfo::Swap)
                            .with_timeout(self.config.timeout),
                    });
                }
                HandlerCommand::AckPseudosettle { request_id, ack } => {
                    // TODO: Store responders and look up by request_id
                    debug!(%request_id, amount = %ack.amount, "Ack pseudosettle (responder not yet implemented)");
                }
            }
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
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
            // Handle inbound protocol completions
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: output,
                ..
            }) => {
                self.handle_inbound_output(output);
            }

            // Handle outbound protocol completions
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: output,
                info,
                ..
            }) => {
                self.handle_outbound_output(output, info);
            }

            // Handle dial (outbound) errors
            ConnectionEvent::DialUpgradeError(e) => {
                let protocol = match &e.info {
                    ClientOutboundInfo::Pricing => {
                        self.pricing_outbound_pending = false;
                        "pricing"
                    }
                    ClientOutboundInfo::Retrieval { .. } => "retrieval",
                    ClientOutboundInfo::Pushsync { .. } => "pushsync",
                    ClientOutboundInfo::Pseudosettle { .. } => "pseudosettle",
                    ClientOutboundInfo::Swap => "swap",
                };
                warn!(protocol, error = %e.error, "Client dial upgrade error");
                self.pending_events.push_back(HandlerEvent::Error {
                    overlay: self.overlay(),
                    protocol,
                    error: e.error.to_string(),
                });
            }

            // Handle listen (inbound) errors
            ConnectionEvent::ListenUpgradeError(e) => {
                warn!(error = %e.error, "Client listen upgrade error");
                self.pending_events.push_back(HandlerEvent::Error {
                    overlay: self.overlay(),
                    protocol: "unknown",
                    error: e.error.to_string(),
                });
            }

            _ => {}
        }
    }
}

impl SwarmClientHandler {
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
                    // TODO: Store responder for later ack
                }
            }
            ClientInboundOutput::Swap(cheque, headers) => {
                if let Some(overlay) = self.overlay() {
                    debug!(%overlay, peer_rate = %headers.exchange_rate, "Received swap cheque");
                    self.pending_events.push_back(HandlerEvent::ChequeReceived {
                        overlay,
                        cheque,
                        peer_rate: headers.exchange_rate,
                    });
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
                if let Some(overlay) = self.overlay() {
                    debug!(%overlay, %address, "Received chunk delivery");
                    self.pending_events.push_back(HandlerEvent::ChunkReceived {
                        overlay,
                        address,
                        data: delivery.data.clone(),
                        stamp: delivery.stamp.clone(),
                    });
                }
            }
            (ClientOutboundOutput::Pushsync(receipt), ClientOutboundInfo::Pushsync { address }) => {
                if let Some(overlay) = self.overlay() {
                    debug!(%overlay, %address, "Received pushsync receipt");
                    self.on_pushsync_receipt(receipt);
                }
            }
            (
                ClientOutboundOutput::Pseudosettle(ack),
                ClientOutboundInfo::Pseudosettle { amount: _ },
            ) => {
                if let Some(overlay) = self.overlay() {
                    debug!(%overlay, ack_amount = %ack.amount, "Pseudosettle sent");
                    self.pending_events
                        .push_back(HandlerEvent::PseudosettleSent { overlay, ack });
                }
            }
            (ClientOutboundOutput::Swap(headers), ClientOutboundInfo::Swap) => {
                if let Some(overlay) = self.overlay() {
                    debug!(%overlay, peer_rate = %headers.exchange_rate, "Swap cheque sent");
                    self.pending_events.push_back(HandlerEvent::ChequeSent {
                        overlay,
                        peer_rate: headers.exchange_rate,
                    });
                }
            }
            // Mismatched output/info - should not happen
            (output, info) => {
                warn!(?output, ?info, "Mismatched outbound output and info");
            }
        }
    }
}
