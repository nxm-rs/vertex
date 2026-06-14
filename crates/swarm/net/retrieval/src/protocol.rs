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

/// Recoverable-signature size carried inside a single-owner chunk's `data`.
const SOC_SIGNATURE_SIZE: usize = 65;

/// Largest legitimate `data` payload in a retrieval `Delivery`: a single-owner
/// chunk, which is the biggest chunk variant on the wire.
///
/// A single-owner chunk serialises as an [`SPAN_SIZE`] span, a 32-byte
/// ([`HASH_SIZE`]) owner id, a 65-byte recoverable signature, and a
/// [`DEFAULT_BODY_SIZE`] body. A content chunk (span plus body) is strictly
/// smaller, so this bound covers both.
const MAX_CHUNK_DATA_SIZE: usize = SPAN_SIZE + HASH_SIZE + SOC_SIGNATURE_SIZE + DEFAULT_BODY_SIZE;

/// Protobuf framing allowance: field tags, length varints, and the outer
/// length-delimited frame prefix across all fields, rounded up generously.
const PROTOBUF_FRAMING: usize = 64;

/// Maximum size of a retrieval `Delivery` frame, derived from the chunk-size
/// constants rather than picked as a round number.
///
/// The retrieval `Delivery` carries `data` plus `stamp` ([`STAMP_SIZE`]) and no
/// address of its own (the requester already knows it). The largest legitimate
/// frame is therefore the largest chunk payload ([`MAX_CHUNK_DATA_SIZE`]) plus a
/// full stamp plus the protobuf framing allowance.
///
/// The arithmetic with the current constants:
/// `8 + 32 + 65 + 4096` data `+ 113` stamp `+ 64` framing `= 4378` bytes, well
/// under 16 KiB. This is a local accept-limit (a DoS guard): the codec rejects
/// an oversized frame at its length prefix, before buffering the body, so an
/// adversarial peer cannot force a transient allocation larger than this. The
/// bound is exact so a conformant peer never trips it and an adversarial frame
/// is capped tightly. Tightening the accept limit is not wire-visible.
const MAX_DELIVERY_SIZE: usize = MAX_CHUNK_DATA_SIZE + STAMP_SIZE + PROTOBUF_FRAMING;

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
            let codec = RequestCodec::new(MAX_DELIVERY_SIZE);
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
                    DeliveryCodec::new(MAX_DELIVERY_SIZE, request.address),
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
    /// Send a successful delivery carrying the chunk.
    ///
    /// The delivery ships the chunk `data` only: the stamp is never put on the
    /// wire, so the `stamp` argument is accepted for call-site symmetry but
    /// dropped at encode. The requester validates the chunk against its address
    /// (BMT hash for content, owner plus signature for single-owner), which is
    /// independent of the stamp.
    pub async fn send_chunk(
        mut self,
        chunk: nectar_primitives::AnyChunk,
        stamp: Option<vertex_swarm_primitives::Stamp>,
    ) -> Result<(), RetrievalError> {
        debug!("Retrieval: Sending chunk delivery");
        self.framed.send(Delivery::chunk(chunk, stamp)).await
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
            let request_codec = RequestCodec::new(MAX_DELIVERY_SIZE);
            let mut framed = Framed::new(stream.into_inner(), request_codec);

            debug!(chunk_address = %self.address, "Retrieval: Sending chunk request");
            framed.send(Request::new(self.address)).await?;

            // Switch to delivery codec and read response. The codec is given the
            // requested address so it can reconstruct and validate the chunk;
            // the retrieval wire frame carries no address of its own.
            // Use into_parts() to preserve any buffered data across the codec switch.
            let parts = framed.into_parts();
            let delivery_codec = DeliveryCodec::new(MAX_DELIVERY_SIZE, self.address);
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

#[cfg(test)]
mod tests {
    use alloy_primitives::{B256, Signature};
    use alloy_signer_local::PrivateKeySigner;
    use asynchronous_codec::{Decoder, Encoder};
    use bytes::BytesMut;
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, SingleOwnerChunk};
    use vertex_swarm_primitives::StampedChunk;

    use super::*;

    /// A stamp with a full-size (113-byte) wire form: a real recoverable
    /// signature plus the fixed batch id, indices, and timestamp.
    fn full_stamp() -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xab), 11, 22, 33, sig)
    }

    /// The largest legitimate delivery: a single-owner chunk (the biggest chunk
    /// variant) carrying a full `DEFAULT_BODY_SIZE` body, paired with a full
    /// stamp. Its `data` field is exactly [`MAX_CHUNK_DATA_SIZE`].
    fn maximal_delivery() -> StampedChunk {
        let signer = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x11)).expect("valid signer");
        let body = vec![0x5au8; DEFAULT_BODY_SIZE];
        let soc = SingleOwnerChunk::new(B256::repeat_byte(0x22), body, &signer).expect("valid soc");
        let chunk: AnyChunk = soc.into();
        StampedChunk::new(chunk, full_stamp())
    }

    /// A maximal legitimate delivery (full SOC body plus a full stamp) must
    /// encode within the cap and decode successfully. This pins the conformance
    /// floor: the cap is never below the largest delivery a peer may send.
    #[test]
    fn maximal_legitimate_delivery_decodes_within_the_cap() {
        let stamped = maximal_delivery();
        let address = *stamped.address();

        // The chunk `data` field alone is the largest chunk payload.
        let wire_data_len = stamped.chunk().clone().into_bytes().len();
        assert_eq!(
            wire_data_len, MAX_CHUNK_DATA_SIZE,
            "the SOC body must be the maximal chunk payload"
        );

        let mut enc = DeliveryCodec::new(MAX_DELIVERY_SIZE, address);
        let mut buf = BytesMut::new();
        enc.encode(Delivery::success(stamped), &mut buf)
            .expect("maximal delivery must encode within the cap");
        assert!(
            buf.len() <= MAX_DELIVERY_SIZE,
            "encoded maximal delivery ({} bytes) must fit the cap ({} bytes)",
            buf.len(),
            MAX_DELIVERY_SIZE
        );

        let mut dec = DeliveryCodec::new(MAX_DELIVERY_SIZE, address);
        let decoded = dec
            .decode(&mut buf)
            .expect("decode must not error")
            .expect("frame must decode");
        match decoded {
            Delivery::Chunk { chunk, .. } => assert_eq!(*chunk.address(), address),
            Delivery::Error => panic!("expected a chunk, got a failure"),
        }
    }

    /// A frame whose length prefix declares more than [`MAX_DELIVERY_SIZE`] must
    /// be rejected at the prefix, before the body is buffered. The body bytes are
    /// deliberately absent: a correct cap rejects on the varint alone, so no
    /// transient allocation of the oversized body is ever forced.
    #[test]
    fn oversized_frame_is_rejected_before_buffering() {
        let address = ChunkAddress::new([0x42; 32]);
        let mut dec = DeliveryCodec::new(MAX_DELIVERY_SIZE, address);

        // Encode only the length prefix: a varint declaring one byte over the
        // cap, with no body following.
        let mut buf = BytesMut::new();
        let mut declared = MAX_DELIVERY_SIZE + 1;
        while declared >= 0x80 {
            buf.extend_from_slice(&[(declared as u8 & 0x7f) | 0x80]);
            declared >>= 7;
        }
        buf.extend_from_slice(&[declared as u8]);

        let err = dec
            .decode(&mut buf)
            .expect_err("an over-cap length prefix must be rejected");
        assert!(
            matches!(err, RetrievalError::Protobuf(_)),
            "over-cap frame must surface as a protobuf/codec error, got {err:?}"
        );
    }

    /// A frame whose declared length is exactly the cap is accepted by the
    /// length check (it then waits for the body), confirming the boundary is
    /// inclusive and a conformant maximal frame is never rejected at the prefix.
    #[test]
    fn frame_at_the_cap_passes_the_length_check() {
        let address = ChunkAddress::new([0x42; 32]);
        let mut dec = DeliveryCodec::new(MAX_DELIVERY_SIZE, address);

        // A varint declaring exactly the cap, with no body yet: the decoder must
        // not error, it must return `None` awaiting more bytes.
        let mut buf = BytesMut::new();
        let mut declared = MAX_DELIVERY_SIZE;
        while declared >= 0x80 {
            buf.extend_from_slice(&[(declared as u8 & 0x7f) | 0x80]);
            declared >>= 7;
        }
        buf.extend_from_slice(&[declared as u8]);

        let decoded = dec
            .decode(&mut buf)
            .expect("a frame declaring exactly the cap must pass the length check");
        assert!(
            decoded.is_none(),
            "no body present yet, decoder must await more bytes rather than yield a frame"
        );
    }
}
