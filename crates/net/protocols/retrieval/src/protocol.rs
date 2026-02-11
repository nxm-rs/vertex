//! Protocol upgrade for retrieval.
//!
//! Implements HeaderedInbound/HeaderedOutbound traits - headers are automatic.
//!
//! # Protocol Flow
//!
//! Retrieval is a request/response protocol:
//! - **Outbound (requester)**: Send Request, receive Delivery
//! - **Inbound (responder)**: Receive Request, send Delivery

use asynchronous_codec::Framed;
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use nectar_primitives::ChunkAddress;
use tracing::{Instrument, debug};
use vertex_net_headers::{HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound};

use crate::{
    PROTOCOL_NAME,
    codec::{Delivery, DeliveryCodec, Request, RequestCodec},
    error::RetrievalError,
};

/// Maximum size of a retrieval message (chunk + stamp + overhead).
const MAX_MESSAGE_SIZE: usize = 5 * 1024 * 1024; // 5 MB

/// Retrieval inbound: receives a chunk request from remote.
#[derive(Debug, Clone)]
pub struct RetrievalInboundInner;

impl HeaderedInbound for RetrievalInboundInner {
    type Output = (Request, RetrievalResponder);
    type Error = RetrievalError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        let span = tracing::info_span!("retrieval_receive");
        Box::pin(
            async move {
                let codec = RequestCodec::new(MAX_MESSAGE_SIZE);
                let mut framed = Framed::new(stream.into_inner(), codec);

                debug!("Retrieval: Reading chunk request");
                let request = framed
                    .try_next()
                    .await?
                    .ok_or(RetrievalError::ConnectionClosed)?;

                tracing::Span::current().record("chunk_address", tracing::field::display(&request.address));

                // Return the request and a responder to send the delivery
                let responder = RetrievalResponder {
                    framed: Framed::new(framed.into_inner(), DeliveryCodec::new(MAX_MESSAGE_SIZE)),
                };

                Ok((request, responder))
            }
            .instrument(span),
        )
    }
}

/// Handle for sending a delivery response.
pub struct RetrievalResponder {
    framed: Framed<libp2p::Stream, DeliveryCodec>,
}

impl RetrievalResponder {
    /// Send a successful delivery with chunk data.
    pub async fn send_chunk(
        mut self,
        data: bytes::Bytes,
        stamp: bytes::Bytes,
    ) -> Result<(), RetrievalError> {
        debug!("Retrieval: Sending chunk delivery");
        self.framed.send(Delivery::success(data, stamp)).await
    }

    /// Send an error response.
    pub async fn send_error(mut self, error: impl Into<String>) -> Result<(), RetrievalError> {
        debug!("Retrieval: Sending error delivery");
        self.framed.send(Delivery::error(error)).await
    }
}

/// Retrieval outbound: requests a chunk from remote.
#[derive(Debug, Clone)]
pub struct RetrievalOutboundInner {
    address: ChunkAddress,
}

impl RetrievalOutboundInner {
    /// Create a new outbound request for the given chunk address.
    pub fn new(address: ChunkAddress) -> Self {
        Self { address }
    }
}

impl HeaderedOutbound for RetrievalOutboundInner {
    type Output = Delivery;
    type Error = RetrievalError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        let chunk_address = self.address;
        let span = tracing::info_span!("retrieval_request", %chunk_address);
        Box::pin(
            async move {
                // Send the request
                let request_codec = RequestCodec::new(MAX_MESSAGE_SIZE);
                let mut framed = Framed::new(stream.into_inner(), request_codec);

                debug!(address = %self.address, "Retrieval: Sending chunk request");
                framed.send(Request::new(self.address)).await?;

                // Switch to delivery codec and read response
                let delivery_codec = DeliveryCodec::new(MAX_MESSAGE_SIZE);
                let mut framed = Framed::new(framed.into_inner(), delivery_codec);

                debug!("Retrieval: Reading delivery response");
                framed
                    .try_next()
                    .await?
                    .ok_or(RetrievalError::ConnectionClosed)
            }
            .instrument(span),
        )
    }
}

/// Inbound protocol type for handler.
pub type RetrievalInboundProtocol = Inbound<RetrievalInboundInner>;

/// Outbound protocol type for handler.
pub type RetrievalOutboundProtocol = Outbound<RetrievalOutboundInner>;

/// Create an inbound protocol handler.
pub fn inbound() -> RetrievalInboundProtocol {
    Inbound::new(RetrievalInboundInner)
}

/// Create an outbound protocol handler for the given chunk address.
pub fn outbound(address: ChunkAddress) -> RetrievalOutboundProtocol {
    Outbound::new(RetrievalOutboundInner::new(address))
}
