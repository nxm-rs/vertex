//! Combined protocol upgrades for client handler.
//!
//! This module provides multi-protocol support for the client handler,
//! combining pricing, retrieval, and pushsync into a single `InboundUpgrade`.
//!
//! # Architecture
//!
//! The client handler needs to accept multiple inbound protocols:
//! - Pricing: Payment threshold exchange (symmetric - both peers announce)
//! - Retrieval: Chunk request/response (full nodes only)
//! - Pushsync: Chunk push with receipt (full nodes only)
//!
//! We use a custom `ClientInboundUpgrade` that implements `UpgradeInfo`
//! with all protocol names and dispatches based on the negotiated protocol.

use alloy_primitives::U256;
use futures::future::BoxFuture;
use libp2p::{InboundUpgrade, OutboundUpgrade, Stream, core::UpgradeInfo};
use thiserror::Error;
use vertex_swarm_bandwidth_chequebook::SignedCheque;
use vertex_net_headers::ProtocolError;
use vertex_net_pricing::{
    AnnouncePaymentThreshold, PROTOCOL_NAME as PRICING_PROTOCOL, PricingInboundProtocol,
    PricingOutboundProtocol,
};
use vertex_net_pseudosettle::{
    PROTOCOL_NAME as PSEUDOSETTLE_PROTOCOL, Payment, PaymentAck, PseudosettleInboundResult,
};
use vertex_net_pushsync::{
    Delivery as PushsyncDelivery, PROTOCOL_NAME as PUSHSYNC_PROTOCOL, PushsyncInboundProtocol,
    PushsyncOutboundProtocol, PushsyncResponder, Receipt as PushsyncReceipt,
};
use vertex_net_retrieval::{
    Delivery as RetrievalDelivery, PROTOCOL_NAME as RETRIEVAL_PROTOCOL,
    Request as RetrievalRequest, RetrievalInboundProtocol, RetrievalOutboundProtocol,
    RetrievalResponder,
};
use vertex_net_swap::{PROTOCOL_NAME as SWAP_PROTOCOL, SettlementHeaders};
use vertex_primitives::ChunkAddress;

/// Errors from client protocol upgrades.
#[derive(Debug, Error)]
pub enum ClientUpgradeError {
    /// Pricing protocol error.
    #[error("pricing error: {0}")]
    Pricing(#[source] ProtocolError),

    /// Retrieval protocol error.
    #[error("retrieval error: {0}")]
    Retrieval(#[source] ProtocolError),

    /// Pushsync protocol error.
    #[error("pushsync error: {0}")]
    Pushsync(#[source] ProtocolError),

    /// Pseudosettle protocol error.
    #[error("pseudosettle error: {0}")]
    Pseudosettle(#[source] ProtocolError),

    /// Swap protocol error.
    #[error("swap error: {0}")]
    Swap(#[source] ProtocolError),

    /// Unknown protocol negotiated.
    #[error("unknown protocol: {0}")]
    UnknownProtocol(String),
}

/// Output from a client inbound upgrade.
pub enum ClientInboundOutput {
    /// Received pricing threshold.
    Pricing(AnnouncePaymentThreshold),
    /// Received retrieval request (with responder to send delivery).
    Retrieval(RetrievalRequest, RetrievalResponder),
    /// Received pushsync delivery (with responder to send receipt).
    Pushsync(PushsyncDelivery, PushsyncResponder),
    /// Received pseudosettle payment (with responder to send ack).
    Pseudosettle(PseudosettleInboundResult),
    /// Received swap cheque with peer's exchange rate.
    Swap(SignedCheque, SettlementHeaders),
}

impl std::fmt::Debug for ClientInboundOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pricing(threshold) => f.debug_tuple("Pricing").field(threshold).finish(),
            Self::Retrieval(request, _) => f
                .debug_tuple("Retrieval")
                .field(request)
                .field(&"<responder>")
                .finish(),
            Self::Pushsync(delivery, _) => f
                .debug_tuple("Pushsync")
                .field(delivery)
                .field(&"<responder>")
                .finish(),
            Self::Pseudosettle(result) => f
                .debug_tuple("Pseudosettle")
                .field(&result.payment)
                .field(&"<responder>")
                .finish(),
            Self::Swap(cheque, headers) => f
                .debug_tuple("Swap")
                .field(cheque)
                .field(headers)
                .finish(),
        }
    }
}

/// Combined inbound upgrade for client protocols.
///
/// Advertises pricing, retrieval, and pushsync protocols and dispatches
/// to the appropriate handler based on the negotiated protocol.
#[derive(Clone, Debug, Default)]
pub struct ClientInboundUpgrade;

impl ClientInboundUpgrade {
    /// Create a new client inbound upgrade.
    pub fn new() -> Self {
        Self
    }
}

impl UpgradeInfo for ClientInboundUpgrade {
    type Info = &'static str;
    type InfoIter = std::vec::IntoIter<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        vec![
            PRICING_PROTOCOL,
            RETRIEVAL_PROTOCOL,
            PUSHSYNC_PROTOCOL,
            PSEUDOSETTLE_PROTOCOL,
            SWAP_PROTOCOL,
        ]
        .into_iter()
    }
}

impl InboundUpgrade<Stream> for ClientInboundUpgrade {
    type Output = ClientInboundOutput;
    type Error = ClientUpgradeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            match info {
                PRICING_PROTOCOL => {
                    let pricing: PricingInboundProtocol = vertex_net_pricing::inbound();
                    let threshold = pricing
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pricing)?;
                    Ok(ClientInboundOutput::Pricing(threshold))
                }
                RETRIEVAL_PROTOCOL => {
                    let retrieval: RetrievalInboundProtocol = vertex_net_retrieval::inbound();
                    let (request, responder) = retrieval
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Retrieval)?;
                    Ok(ClientInboundOutput::Retrieval(request, responder))
                }
                PUSHSYNC_PROTOCOL => {
                    let pushsync: PushsyncInboundProtocol = vertex_net_pushsync::inbound();
                    let (delivery, responder) = pushsync
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pushsync)?;
                    Ok(ClientInboundOutput::Pushsync(delivery, responder))
                }
                PSEUDOSETTLE_PROTOCOL => {
                    let protocol = vertex_net_pseudosettle::inbound();
                    let result = protocol
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pseudosettle)?;
                    Ok(ClientInboundOutput::Pseudosettle(result))
                }
                SWAP_PROTOCOL => {
                    // Use a default rate for inbound - actual rate comes from handler config
                    let our_rate = U256::ZERO;
                    let protocol = vertex_net_swap::inbound(our_rate);
                    let (cheque, headers) = protocol
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Swap)?;
                    Ok(ClientInboundOutput::Swap(cheque, headers))
                }
                other => Err(ClientUpgradeError::UnknownProtocol(other.to_string())),
            }
        })
    }
}

/// Type of outbound request for client protocols.
#[derive(Debug, Clone)]
pub enum ClientOutboundRequest {
    /// Announce payment threshold.
    Pricing(AnnouncePaymentThreshold),
    /// Request a chunk.
    Retrieval(ChunkAddress),
    /// Push a chunk for storage.
    Pushsync(PushsyncDelivery),
    /// Send pseudosettle payment.
    Pseudosettle(Payment),
    /// Send swap cheque with our exchange rate.
    Swap { cheque: SignedCheque, our_rate: U256 },
}

/// Output from a client outbound upgrade.
#[derive(Debug)]
pub enum ClientOutboundOutput {
    /// Pricing announcement sent successfully.
    Pricing,
    /// Received chunk delivery.
    Retrieval(RetrievalDelivery),
    /// Received receipt.
    Pushsync(PushsyncReceipt),
    /// Received pseudosettle ack.
    Pseudosettle(PaymentAck),
    /// Received swap peer headers (exchange rate).
    Swap(SettlementHeaders),
}

/// Combined outbound upgrade for client protocols.
///
/// Unlike inbound, outbound requests know which protocol to use.
#[derive(Clone, Debug)]
pub struct ClientOutboundUpgrade {
    request: ClientOutboundRequest,
}

impl ClientOutboundUpgrade {
    /// Create a new pricing outbound upgrade.
    pub fn pricing(threshold: AnnouncePaymentThreshold) -> Self {
        Self {
            request: ClientOutboundRequest::Pricing(threshold),
        }
    }

    /// Create a new retrieval outbound upgrade.
    pub fn retrieval(address: ChunkAddress) -> Self {
        Self {
            request: ClientOutboundRequest::Retrieval(address),
        }
    }

    /// Create a new pushsync outbound upgrade.
    pub fn pushsync(delivery: PushsyncDelivery) -> Self {
        Self {
            request: ClientOutboundRequest::Pushsync(delivery),
        }
    }

    /// Create a new pseudosettle outbound upgrade.
    pub fn pseudosettle(payment: Payment) -> Self {
        Self {
            request: ClientOutboundRequest::Pseudosettle(payment),
        }
    }

    /// Create a new swap outbound upgrade.
    pub fn swap(cheque: SignedCheque, our_rate: U256) -> Self {
        Self {
            request: ClientOutboundRequest::Swap { cheque, our_rate },
        }
    }

    /// Get the protocol name for this request.
    fn protocol_name(&self) -> &'static str {
        match &self.request {
            ClientOutboundRequest::Pricing(_) => PRICING_PROTOCOL,
            ClientOutboundRequest::Retrieval(_) => RETRIEVAL_PROTOCOL,
            ClientOutboundRequest::Pushsync(_) => PUSHSYNC_PROTOCOL,
            ClientOutboundRequest::Pseudosettle(_) => PSEUDOSETTLE_PROTOCOL,
            ClientOutboundRequest::Swap { .. } => SWAP_PROTOCOL,
        }
    }
}

impl UpgradeInfo for ClientOutboundUpgrade {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(self.protocol_name())
    }
}

impl OutboundUpgrade<Stream> for ClientOutboundUpgrade {
    type Output = ClientOutboundOutput;
    type Error = ClientUpgradeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            match self.request {
                ClientOutboundRequest::Pricing(threshold) => {
                    let pricing: PricingOutboundProtocol = vertex_net_pricing::outbound(threshold);
                    pricing
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pricing)?;
                    Ok(ClientOutboundOutput::Pricing)
                }
                ClientOutboundRequest::Retrieval(address) => {
                    let retrieval: RetrievalOutboundProtocol =
                        vertex_net_retrieval::outbound(address);
                    let delivery = retrieval
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Retrieval)?;
                    Ok(ClientOutboundOutput::Retrieval(delivery))
                }
                ClientOutboundRequest::Pushsync(delivery) => {
                    let pushsync: PushsyncOutboundProtocol =
                        vertex_net_pushsync::outbound(delivery);
                    let receipt = pushsync
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pushsync)?;
                    Ok(ClientOutboundOutput::Pushsync(receipt))
                }
                ClientOutboundRequest::Pseudosettle(payment) => {
                    let protocol = vertex_net_pseudosettle::outbound(payment);
                    let ack = protocol
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pseudosettle)?;
                    Ok(ClientOutboundOutput::Pseudosettle(ack))
                }
                ClientOutboundRequest::Swap { cheque, our_rate } => {
                    let protocol = vertex_net_swap::outbound(cheque, our_rate);
                    let headers = protocol
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Swap)?;
                    Ok(ClientOutboundOutput::Swap(headers))
                }
            }
        })
    }
}

/// Information about an outbound request, used for correlating responses.
#[derive(Debug, Clone)]
pub enum ClientOutboundInfo {
    /// Pricing announcement.
    Pricing,
    /// Retrieval request with chunk address.
    Retrieval { address: ChunkAddress },
    /// Pushsync request with chunk address.
    Pushsync { address: ChunkAddress },
    /// Pseudosettle payment with amount.
    Pseudosettle { amount: U256 },
    /// Swap cheque sent.
    Swap,
}
