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
use nectar_postage::STAMP_SIZE;
use nectar_primitives::{
    ChunkAddress,
    bmt::{DEFAULT_BODY_SIZE, HASH_SIZE, SPAN_SIZE},
};
use tracing::debug;
use vertex_swarm_net_headers::{
    HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound,
};

use crate::{
    PROTOCOL_NAME,
    codec::{Delivery, DeliveryCodec, Request, RequestCodec},
    error::RetrievalError,
};

/// Maximum size of a retrieval message, derived from the chunk-size constants.
///
/// The largest legitimate `data` payload is a single-owner chunk: an
/// [`SPAN_SIZE`] span, a 32-byte ([`HASH_SIZE`]) owner id, a 65-byte recoverable
/// signature, and a [`DEFAULT_BODY_SIZE`] body. The retrieval `Delivery` frames
/// that as `data` + `stamp` ([`STAMP_SIZE`]) with no address of its own. A small
/// fixed allowance covers protobuf field tags, length varints, and the outer
/// length-delimited frame prefix.
///
/// The arithmetic with the current constants:
/// `8 + 32 + 65 + 4096` data `+ 113` stamp `+ 64` framing `= 4378` bytes, well
/// under 16 KiB. The bound is exact rather than a round number so a conformant
/// peer never trips it and an adversarial frame (and any transient field
/// allocation it forces) is capped tightly. Rejecting larger frames is not
/// wire-visible.
const SOC_SIGNATURE_SIZE: usize = 65;
const MAX_CHUNK_DATA_SIZE: usize = SPAN_SIZE + HASH_SIZE + SOC_SIGNATURE_SIZE + DEFAULT_BODY_SIZE;
/// Protobuf framing allowance: field tags, length varints, and the outer
/// length-delimited frame prefix across all fields, rounded up generously.
const PROTOBUF_FRAMING: usize = 64;
const MAX_MESSAGE_SIZE: usize = MAX_CHUNK_DATA_SIZE + STAMP_SIZE + PROTOBUF_FRAMING;

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
        Box::pin(async move {
            let codec = RequestCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Retrieval: Reading chunk request");
            let request = framed
                .try_next()
                .await?
                .ok_or(RetrievalError::ConnectionClosed)?;

            debug!(chunk_address = %request.address, "Retrieval: received request");

            // Return the request and a responder to send the delivery.
            // Use into_parts() to preserve any buffered data across the codec switch.
            // The responder only encodes deliveries, so the codec's expected
            // address (used on decode) is the requested address for symmetry.
            let parts = framed.into_parts();
            let responder = RetrievalResponder {
                framed: Framed::new(
                    parts.io,
                    DeliveryCodec::new(MAX_MESSAGE_SIZE, request.address),
                ),
            };

            Ok((request, responder))
        })
    }
}

/// Handle for sending a delivery response.
pub struct RetrievalResponder {
    framed: Framed<libp2p::Stream, DeliveryCodec>,
}

impl RetrievalResponder {
    /// Send a successful delivery with the stamped chunk.
    pub async fn send_chunk(
        mut self,
        chunk: vertex_swarm_primitives::StampedChunk,
    ) -> Result<(), RetrievalError> {
        debug!("Retrieval: Sending chunk delivery");
        self.framed.send(Delivery::success(chunk)).await
    }

    /// Signal a failure by resetting the stream (no frame is sent).
    pub fn send_error(self) {
        // We never put a placeholder or remote-controlled error on the wire. The
        // requester reads the reset (or EOF) as a failed request. Dropping the
        // framed stream resets the substream at the muxer.
        debug!("Retrieval: resetting stream to signal failure");
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
        Box::pin(async move {
            // Send the request
            let request_codec = RequestCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), request_codec);

            debug!(chunk_address = %self.address, "Retrieval: Sending chunk request");
            framed.send(Request::new(self.address)).await?;

            // Switch to delivery codec and read response. The codec is given the
            // requested address so it can reconstruct and validate the chunk;
            // the retrieval wire frame carries no address of its own.
            // Use into_parts() to preserve any buffered data across the codec switch.
            let parts = framed.into_parts();
            let delivery_codec = DeliveryCodec::new(MAX_MESSAGE_SIZE, self.address);
            let mut framed = Framed::new(parts.io, delivery_codec);

            debug!("Retrieval: Reading delivery response");
            framed
                .try_next()
                .await?
                .ok_or(RetrievalError::ConnectionClosed)
        })
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
