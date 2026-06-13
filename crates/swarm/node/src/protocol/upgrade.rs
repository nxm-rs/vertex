//! Combined protocol upgrades for client handler.
//!
//! This module provides multi-protocol support for the client handler,
//! combining pricing, retrieval, and pushsync into a single `InboundUpgrade`.
//!
//! # Architecture
//!
//! The client handler needs to accept multiple inbound protocols:
//! - Pricing: Payment threshold exchange (symmetric - both peers announce)
//! - Pseudosettle: Bandwidth settlement (symmetric)
//! - Retrieval: Chunk request/response (Storers only)
//! - Pushsync: Chunk push with receipt (Storers only)
//!
//! We use a custom `ClientInboundUpgrade` that implements `UpgradeInfo`
//! with all protocol names and dispatches based on the negotiated protocol.

use alloy_primitives::U256;
use futures::future::BoxFuture;
use libp2p::{InboundUpgrade, OutboundUpgrade, Stream, core::UpgradeInfo};
use nectar_primitives::ChunkAddress;
use thiserror::Error;
use vertex_swarm_net_headers::ProtocolError;
use vertex_swarm_net_pricing::{
    AnnouncePaymentThreshold, PROTOCOL_NAME as PRICING_PROTOCOL, PricingInboundProtocol,
    PricingOutboundProtocol,
};
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
#[cfg(feature = "swap")]
use vertex_swarm_net_swap::{
    PROTOCOL_NAME as SWAP_PROTOCOL, SettlementHeaders, SignedCheque, SwapInboundProtocol,
    SwapOutboundProtocol,
};
/// Errors from client protocol upgrades.
#[derive(Debug, Error)]
pub(crate) enum ClientUpgradeError {
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
    #[cfg(feature = "swap")]
    #[error("swap error: {0}")]
    Swap(#[source] ProtocolError),

    /// Unknown protocol negotiated.
    #[error("unknown protocol: {0}")]
    UnknownProtocol(String),
}

pub(crate) use super::events::FailureKind;

impl ClientUpgradeError {
    /// Classify a retrieval upgrade failure.
    ///
    /// Reaches through the boxed inner error to recover the typed retrieval
    /// codec error and ask it whether the failure was a malformed chunk.
    pub(crate) fn retrieval_failure_kind(&self) -> FailureKind {
        match self {
            Self::Retrieval(ProtocolError::Protocol(inner)) => inner
                .downcast_ref::<vertex_swarm_net_retrieval::RetrievalError>()
                .filter(|e| e.is_invalid_chunk())
                .map_or(FailureKind::Protocol, |_| FailureKind::InvalidChunk),
            _ => FailureKind::Protocol,
        }
    }

    /// Classify a pushsync upgrade failure.
    pub(crate) fn pushsync_failure_kind(&self) -> FailureKind {
        match self {
            Self::Pushsync(ProtocolError::Protocol(inner)) => inner
                .downcast_ref::<vertex_swarm_net_pushsync::PushsyncError>()
                .filter(|e| e.is_invalid_chunk())
                .map_or(FailureKind::Protocol, |_| FailureKind::InvalidChunk),
            _ => FailureKind::Protocol,
        }
    }

    /// Classify a failure on the inbound listen path.
    ///
    /// Used when a peer pushed us a malformed chunk or sent a malformed
    /// retrieval request: the decode rejects it and we attribute the invalid
    /// data to the sender.
    pub(crate) fn inbound_failure_kind(&self) -> FailureKind {
        match self {
            Self::Pushsync(ProtocolError::Protocol(inner)) => inner
                .downcast_ref::<vertex_swarm_net_pushsync::PushsyncError>()
                .filter(|e| e.is_invalid_chunk())
                .map_or(FailureKind::Protocol, |_| FailureKind::InvalidChunk),
            Self::Retrieval(ProtocolError::Protocol(inner)) => inner
                .downcast_ref::<vertex_swarm_net_retrieval::RetrievalError>()
                .filter(|e| e.is_invalid_chunk())
                .map_or(FailureKind::Protocol, |_| FailureKind::InvalidChunk),
            _ => FailureKind::Protocol,
        }
    }
}

/// Output from a client inbound upgrade.
pub(crate) enum ClientInboundOutput {
    /// Received pricing threshold.
    Pricing(AnnouncePaymentThreshold),
    /// Received retrieval request (with responder to send delivery).
    Retrieval(RetrievalRequest, RetrievalResponder),
    /// Received pushsync delivery (with responder to send receipt).
    Pushsync(PushsyncDelivery, PushsyncResponder),
    /// Received pseudosettle payment (with responder to send ack).
    Pseudosettle(PseudosettleInboundResult),
    /// Received a swap cheque with the peer's negotiated headers.
    #[cfg(feature = "swap")]
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
            #[cfg(feature = "swap")]
            Self::Swap(cheque, headers) => {
                f.debug_tuple("Swap").field(cheque).field(headers).finish()
            }
        }
    }
}

/// Combined inbound upgrade for client protocols.
///
/// Advertises pricing, retrieval, pushsync, and pseudosettle based on the
/// handler's state and the local node's role; dispatches to the appropriate
/// per-protocol upgrade after libp2p negotiates a protocol id.
///
/// # State
///
/// - [`Self::new`] (dormant): no protocols advertised. Used before the
///   topology handshake completes; prevents a remote peer from initiating
///   any client protocol before we have verified them.
/// - [`Self::active_for`] (active): advertises a protocol set picked by the
///   local node's [`SwarmNodeType`]. Bootnodes advertise pricing only
///   (listen-only); clients and storers advertise the full set.
#[derive(Clone, Debug, Default)]
pub(crate) struct ClientInboundUpgrade {
    advertised: ProtocolSet,
    /// Our advertised swap exchange rate, sent in the headers exchange.
    #[cfg(feature = "swap")]
    swap_rate: U256,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ProtocolSet {
    /// Dormant: no protocols advertised.
    #[default]
    None,
    /// Bootnode-active: pricing only.
    PricingOnly,
    /// Client/storer-active: pricing + retrieval + pushsync + pseudosettle (+ swap).
    Full,
}

impl ClientInboundUpgrade {
    /// Create a new client inbound upgrade in dormant state.
    pub(crate) fn new() -> Self {
        Self {
            advertised: ProtocolSet::None,
            #[cfg(feature = "swap")]
            swap_rate: U256::ZERO,
        }
    }

    /// Create a new client inbound upgrade in active state for `local_role`.
    /// Bootnodes only advertise pricing; clients and storers advertise the
    /// full client protocol set.
    pub(crate) fn active_for(local_role: vertex_swarm_primitives::SwarmNodeType) -> Self {
        let advertised = match local_role {
            vertex_swarm_primitives::SwarmNodeType::Bootnode => ProtocolSet::PricingOnly,
            vertex_swarm_primitives::SwarmNodeType::Client
            | vertex_swarm_primitives::SwarmNodeType::Storer => ProtocolSet::Full,
        };
        Self {
            advertised,
            #[cfg(feature = "swap")]
            swap_rate: U256::ZERO,
        }
    }

    /// Set the swap exchange rate advertised in the headers exchange.
    #[cfg(feature = "swap")]
    pub(crate) fn with_swap_rate(mut self, rate: U256) -> Self {
        self.swap_rate = rate;
        self
    }
}

impl UpgradeInfo for ClientInboundUpgrade {
    type Info = &'static str;
    type InfoIter = std::vec::IntoIter<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        match self.advertised {
            ProtocolSet::None => Vec::new().into_iter(),
            ProtocolSet::PricingOnly => vec![PRICING_PROTOCOL].into_iter(),
            ProtocolSet::Full => {
                let protocols = vec![
                    PRICING_PROTOCOL,
                    RETRIEVAL_PROTOCOL,
                    PUSHSYNC_PROTOCOL,
                    PSEUDOSETTLE_PROTOCOL,
                    #[cfg(feature = "swap")]
                    SWAP_PROTOCOL,
                ];
                protocols.into_iter()
            }
        }
    }
}

impl InboundUpgrade<Stream> for ClientInboundUpgrade {
    type Output = ClientInboundOutput;
    type Error = ClientUpgradeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        #[cfg(feature = "swap")]
        let swap_rate = self.swap_rate;
        Box::pin(async move {
            match info {
                PRICING_PROTOCOL => {
                    let pricing: PricingInboundProtocol = vertex_swarm_net_pricing::inbound();
                    let threshold = pricing
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pricing)?;
                    Ok(ClientInboundOutput::Pricing(threshold))
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
                #[cfg(feature = "swap")]
                SWAP_PROTOCOL => {
                    let protocol: SwapInboundProtocol = vertex_swarm_net_swap::inbound(swap_rate);
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
pub(crate) enum ClientOutboundRequest {
    /// Announce payment threshold.
    Pricing(AnnouncePaymentThreshold),
    /// Request a chunk.
    Retrieval(ChunkAddress),
    /// Push a chunk for storage.
    Pushsync(PushsyncDelivery),
    /// Send pseudosettle payment.
    Pseudosettle(Payment),
    /// Send a swap cheque with our advertised exchange rate.
    #[cfg(feature = "swap")]
    Swap(SignedCheque, U256),
}

/// Output from a client outbound upgrade.
#[derive(Debug)]
pub(crate) enum ClientOutboundOutput {
    /// Pricing announcement sent successfully.
    Pricing,
    /// Received chunk delivery.
    Retrieval(RetrievalDelivery),
    /// Received receipt.
    Pushsync(PushsyncReceipt),
    /// Received pseudosettle ack.
    Pseudosettle(PaymentAck),
    /// Cheque sent; carries the peer's negotiated headers.
    #[cfg(feature = "swap")]
    Swap(SettlementHeaders),
}

/// Combined outbound upgrade for client protocols.
///
/// Unlike inbound, outbound requests know which protocol to use.
#[derive(Clone, Debug)]
pub(crate) struct ClientOutboundUpgrade {
    request: ClientOutboundRequest,
}

impl ClientOutboundUpgrade {
    /// Create a new pricing outbound upgrade.
    pub(crate) fn pricing(threshold: AnnouncePaymentThreshold) -> Self {
        Self {
            request: ClientOutboundRequest::Pricing(threshold),
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

    /// Create a new swap outbound upgrade that emits a cheque.
    #[cfg(feature = "swap")]
    pub(crate) fn swap(cheque: SignedCheque, our_rate: U256) -> Self {
        Self {
            request: ClientOutboundRequest::Swap(cheque, our_rate),
        }
    }

    /// Get the protocol name for this request.
    fn protocol_name(&self) -> &'static str {
        match &self.request {
            ClientOutboundRequest::Pricing(_) => PRICING_PROTOCOL,
            ClientOutboundRequest::Retrieval(_) => RETRIEVAL_PROTOCOL,
            ClientOutboundRequest::Pushsync(_) => PUSHSYNC_PROTOCOL,
            ClientOutboundRequest::Pseudosettle(_) => PSEUDOSETTLE_PROTOCOL,
            #[cfg(feature = "swap")]
            ClientOutboundRequest::Swap(..) => SWAP_PROTOCOL,
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
                    let pricing: PricingOutboundProtocol =
                        vertex_swarm_net_pricing::outbound(threshold);
                    pricing
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(ClientUpgradeError::Pricing)?;
                    Ok(ClientOutboundOutput::Pricing)
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
                #[cfg(feature = "swap")]
                ClientOutboundRequest::Swap(cheque, our_rate) => {
                    let protocol: SwapOutboundProtocol =
                        vertex_swarm_net_swap::outbound(cheque, our_rate);
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
///
/// Travels with the outbound substream and comes back with its completion
/// (or failure), so it is the natural per-request correlation: retrieval and
/// pushsync carry their caller's response channel here, and the handler
/// resolves it from whichever path terminates the request.
#[derive(Debug)]
pub(crate) enum ClientOutboundInfo {
    /// Pricing announcement.
    Pricing,
    /// Retrieval request with chunk address and the caller's response channel.
    Retrieval {
        address: ChunkAddress,
        response: super::events::RetrievalResponseTx,
        /// When the outbound substream was requested, for latency scoring.
        requested_at: vertex_util_runtime::time::Instant,
    },
    /// Pushsync request with chunk address and the caller's response channel.
    Pushsync {
        address: ChunkAddress,
        response: super::events::PushResponseTx,
        /// When the outbound substream was requested, for latency scoring.
        requested_at: vertex_util_runtime::time::Instant,
    },
    /// Pseudosettle payment with amount.
    Pseudosettle { amount: U256 },
    /// Swap cheque emission.
    #[cfg(feature = "swap")]
    Swap,
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_primitives::SwarmNodeType;

    fn retrieval_protocol_err(inner: vertex_swarm_net_retrieval::RetrievalError) -> ProtocolError {
        ProtocolError::Protocol(Box::new(inner))
    }

    fn pushsync_protocol_err(inner: vertex_swarm_net_pushsync::PushsyncError) -> ProtocolError {
        ProtocolError::Protocol(Box::new(inner))
    }

    #[test]
    fn malformed_retrieval_classifies_as_invalid_chunk() {
        let err = ClientUpgradeError::Retrieval(retrieval_protocol_err(
            vertex_swarm_net_retrieval::RetrievalError::InvalidAddressLength(0),
        ));
        assert_eq!(err.retrieval_failure_kind(), FailureKind::InvalidChunk);
    }

    #[test]
    fn plain_retrieval_error_classifies_as_protocol() {
        let err = ClientUpgradeError::Retrieval(retrieval_protocol_err(
            vertex_swarm_net_retrieval::RetrievalError::ConnectionClosed,
        ));
        assert_eq!(err.retrieval_failure_kind(), FailureKind::Protocol);
    }

    #[test]
    fn malformed_pushsync_classifies_as_invalid_chunk_inbound() {
        let err = ClientUpgradeError::Pushsync(pushsync_protocol_err(
            vertex_swarm_net_pushsync::PushsyncError::InvalidAddressLength(0),
        ));
        assert_eq!(err.pushsync_failure_kind(), FailureKind::InvalidChunk);
        assert_eq!(err.inbound_failure_kind(), FailureKind::InvalidChunk);
    }

    #[test]
    fn pricing_error_is_never_invalid_chunk() {
        let err = ClientUpgradeError::Pricing(ProtocolError::Protocol(Box::new(
            vertex_swarm_net_retrieval::RetrievalError::ConnectionClosed,
        )));
        assert_eq!(err.retrieval_failure_kind(), FailureKind::Protocol);
        assert_eq!(err.inbound_failure_kind(), FailureKind::Protocol);
    }

    #[test]
    fn dormant_advertises_nothing() {
        let upgrade = ClientInboundUpgrade::new();
        let protocols: Vec<_> = upgrade.protocol_info().collect();
        assert!(protocols.is_empty(), "dormant must advertise no protocols");
    }

    #[test]
    fn bootnode_role_advertises_pricing_only() {
        let upgrade = ClientInboundUpgrade::active_for(SwarmNodeType::Bootnode);
        let protocols: Vec<_> = upgrade.protocol_info().collect();
        assert_eq!(protocols, vec![PRICING_PROTOCOL]);
    }

    #[test]
    fn client_role_advertises_full_set() {
        let upgrade = ClientInboundUpgrade::active_for(SwarmNodeType::Client);
        let protocols: Vec<_> = upgrade.protocol_info().collect();
        assert_eq!(
            protocols,
            vec![
                PRICING_PROTOCOL,
                RETRIEVAL_PROTOCOL,
                PUSHSYNC_PROTOCOL,
                PSEUDOSETTLE_PROTOCOL,
                #[cfg(feature = "swap")]
                SWAP_PROTOCOL,
            ]
        );
    }

    #[test]
    fn storer_role_advertises_full_set() {
        let upgrade = ClientInboundUpgrade::active_for(SwarmNodeType::Storer);
        let protocols: Vec<_> = upgrade.protocol_info().collect();
        assert_eq!(
            protocols,
            vec![
                PRICING_PROTOCOL,
                RETRIEVAL_PROTOCOL,
                PUSHSYNC_PROTOCOL,
                PSEUDOSETTLE_PROTOCOL,
                #[cfg(feature = "swap")]
                SWAP_PROTOCOL,
            ]
        );
    }

    #[cfg(feature = "swap")]
    #[test]
    fn full_set_includes_swap_when_enabled() {
        let upgrade = ClientInboundUpgrade::active_for(SwarmNodeType::Client);
        let protocols: Vec<_> = upgrade.protocol_info().collect();
        assert!(protocols.contains(&SWAP_PROTOCOL));
    }
}
