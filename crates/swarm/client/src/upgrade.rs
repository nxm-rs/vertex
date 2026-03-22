//! Combined protocol upgrades for client handler.
//!
//! This module provides multi-protocol support for the client handler,
//! combining credit, retrieval, and pushsync into a single `InboundUpgrade`.
//!
//! # Architecture
//!
//! The client handler needs to accept multiple inbound protocols:
//! - Credit: Credit limit exchange (symmetric -- both peers announce)
//! - Pseudosettle: Bandwidth settlement (symmetric)
//! - Retrieval: Chunk request/response (full nodes only)
//! - Pushsync: Chunk push with receipt (full nodes only)
//!
//! We use a custom `ClientInboundUpgrade` that implements `UpgradeInfo`
//! with all protocol names and dispatches based on the negotiated protocol.

use alloy_primitives::U256;
use futures::future::BoxFuture;
use libp2p::{InboundUpgrade, OutboundUpgrade, Stream, core::UpgradeInfo};
use nectar_primitives::ChunkAddress;
use thiserror::Error;
use vertex_swarm_net_credit::{
    AnnounceCreditLimit, CreditInboundProtocol, CreditOutboundProtocol,
    PROTOCOL_NAME as CREDIT_PROTOCOL,
};
use vertex_swarm_net_headers::ProtocolError;
use vertex_swarm_net_pseudosettle::{
    PROTOCOL_NAME as PSEUDOSETTLE_PROTOCOL, Payment, PaymentAck, PseudosettleInboundResult,
};
use vertex_swarm_net_pushsync::{
    Delivery as PushsyncDelivery, PROTOCOL_NAME as PUSHSYNC_PROTOCOL, PushsyncInboundProtocol,
    PushsyncOutboundProtocol, PushsyncResponder, Receipt as PushsyncReceipt,
};
use vertex_swarm_net_retrieval::{
    Delivery as RetrievalDelivery, PROTOCOL_NAME as RETRIEVAL_PROTOCOL,
    Request as RetrievalRequest, RetrievalInboundProtocol, RetrievalOutboundProtocol,
    RetrievalResponder,
};
/// Errors from client protocol upgrades.
#[derive(Debug, Error)]
pub enum ClientUpgradeError {
    /// Credit protocol error.
    #[error("credit error: {0}")]
    Credit(#[source] ProtocolError),

    /// Retrieval protocol error.
    #[error("retrieval error: {0}")]
    Retrieval(#[source] ProtocolError),

    /// Pushsync protocol error.
    #[error("pushsync error: {0}")]
    Pushsync(#[source] ProtocolError),

    /// Pseudosettle protocol error.
    #[error("pseudosettle error: {0}")]
    Pseudosettle(#[source] ProtocolError),

    /// Unknown protocol negotiated.
    #[error("unknown protocol: {0}")]
    UnknownProtocol(String),
}

/// Output from a client inbound upgrade.
pub enum ClientInboundOutput {
    /// Received credit limit.
    Credit(AnnounceCreditLimit),
    /// Received retrieval request (with responder to send delivery).
    Retrieval(RetrievalRequest, RetrievalResponder),
    /// Received pushsync delivery (with responder to send receipt).
    Pushsync(PushsyncDelivery, PushsyncResponder),
    /// Received pseudosettle payment (with responder to send ack).
    Pseudosettle(PseudosettleInboundResult),
}

impl std::fmt::Debug for ClientInboundOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Credit(limit) => f.debug_tuple("Credit").field(limit).finish(),
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
        }
    }
}

/// Combined inbound upgrade for client protocols.
///
/// Advertises credit, retrieval, and pushsync protocols and dispatches
/// to the appropriate handler based on the negotiated protocol.
///
/// # Dormant State
///
/// When `is_active` is false, no protocols are advertised. This prevents
/// remote peers from initiating client protocols before the handshake is
/// complete. Once the handler is activated (after handshake), protocols
/// are advertised on subsequent inbound substream requests.
#[derive(Clone, Debug, Default)]
pub struct ClientInboundUpgrade {
    /// Whether the handler is active (post-handshake).
    is_active: bool,
}

impl ClientInboundUpgrade {
    /// Create a new client inbound upgrade in dormant state (no protocols advertised).
    pub(crate) fn new() -> Self {
        Self { is_active: false }
    }

    /// Create a new client inbound upgrade in active state (all protocols advertised).
    pub(crate) fn active() -> Self {
        Self { is_active: true }
    }
}

impl UpgradeInfo for ClientInboundUpgrade {
    type Info = &'static str;
    type InfoIter = std::vec::IntoIter<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        if self.is_active {
            vec![
                CREDIT_PROTOCOL,
                RETRIEVAL_PROTOCOL,
                PUSHSYNC_PROTOCOL,
                PSEUDOSETTLE_PROTOCOL,
            ]
            .into_iter()
        } else {
            // In dormant state, don't advertise any client protocols.
            // This prevents remote peers from initiating protocols before handshake.
            vec![].into_iter()
        }
    }
}

impl InboundUpgrade<Stream> for ClientInboundUpgrade {
    type Output = ClientInboundOutput;
    type Error = ClientUpgradeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            match info {
                CREDIT_PROTOCOL => {
                    let credit: CreditInboundProtocol = vertex_swarm_net_credit::inbound();
                    let limit = credit
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Credit)?;
                    Ok(ClientInboundOutput::Credit(limit))
                }
                RETRIEVAL_PROTOCOL => {
                    let retrieval: RetrievalInboundProtocol = vertex_swarm_net_retrieval::inbound();
                    let (request, responder) = retrieval
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Retrieval)?;
                    Ok(ClientInboundOutput::Retrieval(request, responder))
                }
                PUSHSYNC_PROTOCOL => {
                    let pushsync: PushsyncInboundProtocol = vertex_swarm_net_pushsync::inbound();
                    let (delivery, responder) = pushsync
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pushsync)?;
                    Ok(ClientInboundOutput::Pushsync(delivery, responder))
                }
                PSEUDOSETTLE_PROTOCOL => {
                    let protocol = vertex_swarm_net_pseudosettle::inbound();
                    let result = protocol
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pseudosettle)?;
                    Ok(ClientInboundOutput::Pseudosettle(result))
                }
                other => Err(ClientUpgradeError::UnknownProtocol(other.to_string())),
            }
        })
    }
}

/// Type of outbound request for client protocols.
#[derive(Debug, Clone)]
pub(crate) enum ClientOutboundRequest {
    /// Announce credit limit.
    Credit(AnnounceCreditLimit),
    /// Request a chunk.
    Retrieval(ChunkAddress),
    /// Push a chunk for storage.
    Pushsync(PushsyncDelivery),
    /// Send pseudosettle payment.
    Pseudosettle(Payment),
}

/// Output from a client outbound upgrade.
#[derive(Debug)]
pub enum ClientOutboundOutput {
    /// Credit limit announcement sent successfully.
    Credit,
    /// Received chunk delivery.
    Retrieval(RetrievalDelivery),
    /// Received receipt.
    Pushsync(PushsyncReceipt),
    /// Received pseudosettle ack.
    Pseudosettle(PaymentAck),
}

/// Combined outbound upgrade for client protocols.
///
/// Unlike inbound, outbound requests know which protocol to use.
#[derive(Clone, Debug)]
pub struct ClientOutboundUpgrade {
    request: ClientOutboundRequest,
}

impl ClientOutboundUpgrade {
    /// Create a new credit outbound upgrade.
    pub(crate) fn credit(limit: AnnounceCreditLimit) -> Self {
        Self {
            request: ClientOutboundRequest::Credit(limit),
        }
    }

    /// Create a new retrieval outbound upgrade.
    pub(crate) fn retrieval(address: ChunkAddress) -> Self {
        Self {
            request: ClientOutboundRequest::Retrieval(address),
        }
    }

    /// Create a new pushsync outbound upgrade.
    pub(crate) fn pushsync(delivery: PushsyncDelivery) -> Self {
        Self {
            request: ClientOutboundRequest::Pushsync(delivery),
        }
    }

    /// Create a new pseudosettle outbound upgrade.
    pub(crate) fn pseudosettle(payment: Payment) -> Self {
        Self {
            request: ClientOutboundRequest::Pseudosettle(payment),
        }
    }

    /// Get the protocol name for this request.
    fn protocol_name(&self) -> &'static str {
        match &self.request {
            ClientOutboundRequest::Credit(_) => CREDIT_PROTOCOL,
            ClientOutboundRequest::Retrieval(_) => RETRIEVAL_PROTOCOL,
            ClientOutboundRequest::Pushsync(_) => PUSHSYNC_PROTOCOL,
            ClientOutboundRequest::Pseudosettle(_) => PSEUDOSETTLE_PROTOCOL,
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
                ClientOutboundRequest::Credit(limit) => {
                    let credit: CreditOutboundProtocol = vertex_swarm_net_credit::outbound(limit);
                    credit
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Credit)?;
                    Ok(ClientOutboundOutput::Credit)
                }
                ClientOutboundRequest::Retrieval(address) => {
                    let retrieval: RetrievalOutboundProtocol =
                        vertex_swarm_net_retrieval::outbound(address);
                    let delivery = retrieval
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Retrieval)?;
                    Ok(ClientOutboundOutput::Retrieval(delivery))
                }
                ClientOutboundRequest::Pushsync(delivery) => {
                    let pushsync: PushsyncOutboundProtocol =
                        vertex_swarm_net_pushsync::outbound(delivery);
                    let receipt = pushsync
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pushsync)?;
                    Ok(ClientOutboundOutput::Pushsync(receipt))
                }
                ClientOutboundRequest::Pseudosettle(payment) => {
                    let protocol = vertex_swarm_net_pseudosettle::outbound(payment);
                    let ack = protocol
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pseudosettle)?;
                    Ok(ClientOutboundOutput::Pseudosettle(ack))
                }
            }
        })
    }
}

/// Information about an outbound request, used for correlating responses.
#[derive(Debug, Clone)]
pub enum ClientOutboundInfo {
    /// Credit limit announcement.
    Credit,
    /// Retrieval request with chunk address.
    Retrieval { address: ChunkAddress },
    /// Pushsync request with chunk address.
    Pushsync { address: ChunkAddress },
    /// Pseudosettle payment with amount.
    Pseudosettle { amount: U256 },
}
