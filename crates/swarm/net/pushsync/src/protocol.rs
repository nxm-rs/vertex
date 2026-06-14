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
use nectar_postage::STAMP_SIZE;
use nectar_primitives::bmt::{DEFAULT_BODY_SIZE, HASH_SIZE, SPAN_SIZE};
use tracing::debug;
use vertex_swarm_net_headers::{
    HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound,
};

use crate::{
    PROTOCOL_NAME,
    codec::{Delivery, DeliveryCodec, ReceiptCodec, ReceiptResponse, WireReceipt},
    error::PushsyncError,
};

/// Maximum size of a pushsync message, derived from the chunk-size constants.
///
/// The largest legitimate `data` payload is a single-owner chunk: an
/// [`SPAN_SIZE`] span, a 32-byte ([`HASH_SIZE`]) owner id, a 65-byte recoverable
/// signature, and a [`DEFAULT_BODY_SIZE`] body. The pushsync `Delivery` frames
/// that as `address` (32 bytes) + `data` + `stamp` ([`STAMP_SIZE`]). A small
/// fixed allowance covers protobuf field tags, length varints, and the outer
/// length-delimited frame prefix.
///
/// The arithmetic with the current constants:
/// `8 + 32 + 65 + 4096` data `+ 32` address `+ 113` stamp `+ 64` framing
/// `= 4410` bytes, well under 16 KiB. The bound is exact rather than a round
/// number so a conformant peer never trips it and an adversarial frame (and any
/// transient field allocation it forces) is capped tightly. Rejecting larger
/// frames is not wire-visible.
const SOC_SIGNATURE_SIZE: usize = 65;
const MAX_CHUNK_DATA_SIZE: usize = SPAN_SIZE + HASH_SIZE + SOC_SIGNATURE_SIZE + DEFAULT_BODY_SIZE;
/// Protobuf framing allowance: field tags, length varints, and the outer
/// length-delimited frame prefix across all fields, rounded up generously.
const PROTOBUF_FRAMING: usize = 64;
const MAX_MESSAGE_SIZE: usize = HASH_SIZE + MAX_CHUNK_DATA_SIZE + STAMP_SIZE + PROTOBUF_FRAMING;

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

            debug!(chunk_address = %delivery.chunk.address(), "Pushsync: received delivery");

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
    pub async fn send_receipt(mut self, receipt: WireReceipt) -> Result<(), PushsyncError> {
        debug!(address = %receipt.address, "Pushsync: Sending receipt");
        self.framed.send(ReceiptResponse::Stored(receipt)).await
    }

    /// Signal a failure by resetting the stream (no frame is sent).
    pub fn send_error(self) {
        // The reference pusher reads the reset (or EOF) as a failed push at
        // every hop, which sidesteps the forwarder signature-skip entirely: with
        // no receipt to read, there is nothing for a forwarder to misjudge as a
        // success. Dropping the framed stream resets the substream at the muxer.
        debug!("Pushsync: resetting stream to signal failure");
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
    type Output = ReceiptResponse;
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

            debug!(chunk_address = %self.delivery.chunk.address(), "Pushsync: Sending chunk delivery");
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
