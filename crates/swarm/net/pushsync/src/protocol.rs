//! Protocol upgrade for pushsync.
//!
//! Implements HeaderedInbound/HeaderedOutbound traits - headers are automatic.
//!
//! # Protocol Flow
//!
//! Pushsync is a request/response protocol:
//! - **Outbound (pusher)**: Send Delivery, receive Receipt
//! - **Inbound (storer)**: Receive Delivery, send Receipt

use asynchronous_codec::Framed;
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use nectar_primitives::ChunkAddress;
use tracing::debug;
use vertex_swarm_net_headers::{HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound};

use crate::{
    PROTOCOL_NAME,
    codec::{Delivery, DeliveryCodec, Receipt, ReceiptCodec},
    error::PushsyncError,
};

/// Maximum size of a pushsync message (chunk + stamp + overhead).
const MAX_MESSAGE_SIZE: usize = 5 * 1024 * 1024; // 5 MB

/// Pushsync inbound: receives a chunk delivery from remote.
#[derive(Debug, Clone)]
pub struct PushsyncInboundInner;

impl HeaderedInbound for PushsyncInboundInner {
    type Output = (Delivery, PushsyncResponder);
    type Error = PushsyncError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let codec = DeliveryCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Pushsync: Reading chunk delivery");
            let delivery = framed
                .try_next()
                .await?
                .ok_or(PushsyncError::ConnectionClosed)?;

            debug!(chunk_address = %delivery.address, "Pushsync: received delivery");

            // Return the delivery and a responder to send the receipt.
            // Use into_parts() to preserve any buffered data across the codec switch.
            let parts = framed.into_parts();
            let responder = PushsyncResponder {
                framed: Framed::new(parts.io, ReceiptCodec::new(MAX_MESSAGE_SIZE)),
            };

            Ok((delivery, responder))
        })
    }
}

/// Handle for sending a receipt response.
pub struct PushsyncResponder {
    framed: Framed<libp2p::Stream, ReceiptCodec>,
}

impl PushsyncResponder {
    /// Send a successful receipt.
    pub async fn send_receipt(mut self, receipt: Receipt) -> Result<(), PushsyncError> {
        debug!(address = %receipt.address, "Pushsync: Sending receipt");
        self.framed.send(receipt).await
    }

    /// Send an error receipt.
    pub async fn send_error(
        mut self,
        address: ChunkAddress,
        error: impl Into<String>,
    ) -> Result<(), PushsyncError> {
        debug!(%address, "Pushsync: Sending error receipt");
        self.framed.send(Receipt::error(address, error)).await
    }
}

/// Pushsync outbound: pushes a chunk to remote for storage.
#[derive(Debug, Clone)]
pub struct PushsyncOutboundInner {
    delivery: Delivery,
}

impl PushsyncOutboundInner {
    /// Create a new outbound pushsync with the given delivery.
    pub fn new(delivery: Delivery) -> Self {
        Self { delivery }
    }
}

impl HeaderedOutbound for PushsyncOutboundInner {
    type Output = Receipt;
    type Error = PushsyncError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            // Send the delivery
            let delivery_codec = DeliveryCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), delivery_codec);

            debug!(chunk_address = %self.delivery.address, "Pushsync: Sending chunk delivery");
            framed.send(self.delivery).await?;

            // Switch to receipt codec and read response.
            // Use into_parts() to preserve any buffered data across the codec switch.
            let parts = framed.into_parts();
            let receipt_codec = ReceiptCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(parts.io, receipt_codec);

            debug!("Pushsync: Reading receipt");
            framed
                .try_next()
                .await?
                .ok_or(PushsyncError::ConnectionClosed)
        })
    }
}

/// Inbound protocol type for handler.
pub type PushsyncInboundProtocol = Inbound<PushsyncInboundInner>;

/// Outbound protocol type for handler.
pub type PushsyncOutboundProtocol = Outbound<PushsyncOutboundInner>;

/// Create an inbound protocol handler.
pub fn inbound() -> PushsyncInboundProtocol {
    Inbound::new(PushsyncInboundInner)
}

/// Create an outbound protocol handler for the given delivery.
pub fn outbound(delivery: Delivery) -> PushsyncOutboundProtocol {
    Outbound::new(PushsyncOutboundInner::new(delivery))
}
