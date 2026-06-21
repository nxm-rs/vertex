//! Protocol upgrades for pullsync's two headered streams.
//!
//! - **Cursors** ([`PROTOCOL_CURSORS`]): `Syn` then `Ack`.
//! - **Sync** ([`PROTOCOL_SYNC`]): `Get`, `Offer`, `Want`, then one `Delivery`
//!   per set bit, all on one stream. Each phase swaps the codec via [`reframe`],
//!   preserving any bytes already buffered.

use asynchronous_codec::{Decoder, Encoder, Framed};
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use nectar_postage::STAMP_SIZE;
use nectar_primitives::bmt::{DEFAULT_BODY_SIZE, HASH_SIZE, SPAN_SIZE};
use tracing::debug;
use vertex_swarm_net_headers::{
    HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound,
};

use crate::{
    DEFAULT_MAX_PAGE, PROTOCOL_CURSORS, PROTOCOL_SYNC,
    codec::{
        Ack, AckCodec, Delivery, DeliveryCodec, Get, GetCodec, Offer, OfferCodec, Syn, SynCodec,
        Want, WantCodec,
    },
    error::PullsyncError,
};

/// One chunk descriptor on the wire: three 32-byte fields plus per-field
/// protobuf framing.
const DESCRIPTOR_SIZE: usize = 3 * HASH_SIZE + 16;

/// Protobuf framing allowance across a message's fields, rounded up generously.
const PROTOBUF_FRAMING: usize = 64;

/// Recoverable-signature size carried inside a single-owner chunk's `data`.
const SOC_SIGNATURE_SIZE: usize = 65;

/// Largest legitimate chunk `data` payload: a single-owner chunk (span, owner
/// id, signature, body), the biggest chunk variant on the wire.
const MAX_CHUNK_DATA_SIZE: usize = SPAN_SIZE + HASH_SIZE + SOC_SIGNATURE_SIZE + DEFAULT_BODY_SIZE;

/// Accept-limit for the cursor handshake messages (`Syn`, `Ack`). `Ack` carries
/// one `u64` per bin plus the epoch; a generous fixed bound covers the full bin
/// space and framing.
const MAX_HANDSHAKE_SIZE: usize = 8 * (nectar_primitives::Bin::COUNT + 1) + PROTOBUF_FRAMING;

/// Accept-limit for a `Get`: a bin and a start cursor.
const MAX_GET_SIZE: usize = 8 + 8 + PROTOBUF_FRAMING;

/// Accept-limit for an `Offer` page: a full page of descriptors plus the
/// `topmost` cursor and framing. A local DoS guard, not wire-visible.
const MAX_OFFER_SIZE: usize = DEFAULT_MAX_PAGE as usize * DESCRIPTOR_SIZE + 8 + PROTOBUF_FRAMING;

/// Accept-limit for a `Want`: one selection bit per offered chunk, packed into
/// `DEFAULT_MAX_PAGE / 8 + 1` bytes.
const MAX_WANT_SIZE: usize = DEFAULT_MAX_PAGE as usize / 8 + 1 + PROTOBUF_FRAMING;

/// Accept-limit for a `Delivery`: the largest chunk payload, its address, and a
/// full stamp.
const MAX_DELIVERY_SIZE: usize = HASH_SIZE + MAX_CHUNK_DATA_SIZE + STAMP_SIZE + PROTOBUF_FRAMING;

/// Re-frame an existing stream with a new codec, preserving the bytes already
/// read into or queued for the stream across the phase switch.
fn reframe<C1, C2>(framed: Framed<libp2p::Stream, C1>, codec: C2) -> Framed<libp2p::Stream, C2>
where
    C1: Encoder + Decoder,
    C2: Encoder + Decoder,
{
    Framed::from_parts(framed.into_parts().map_codec(|_| codec))
}

// ---------------------------------------------------------------------------
// Cursor handshake
// ---------------------------------------------------------------------------

/// Cursors inbound: read `Syn`, answer with `Ack`.
#[derive(Debug, Clone)]
pub struct CursorsInboundInner;

impl HeaderedInbound for CursorsInboundInner {
    type Output = CursorsResponder;
    type Error = PullsyncError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_CURSORS
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let mut framed = Framed::new(stream.into_inner(), SynCodec::new(MAX_HANDSHAKE_SIZE));
            debug!("Pullsync cursors: reading syn");
            framed
                .try_next()
                .await?
                .ok_or(PullsyncError::ConnectionClosed)?;
            let framed = reframe(framed, AckCodec::new(MAX_HANDSHAKE_SIZE));
            Ok(CursorsResponder { framed })
        })
    }
}

/// Handle for replying to a cursor handshake with the local cursors.
pub struct CursorsResponder {
    framed: Framed<libp2p::Stream, AckCodec>,
}

impl CursorsResponder {
    /// Send the per-bin cursors and epoch, closing the handshake.
    pub async fn send_ack(mut self, ack: Ack) -> Result<(), PullsyncError> {
        debug!(epoch = ack.epoch, "Pullsync cursors: sending ack");
        self.framed.send(ack).await
    }
}

/// Cursors outbound: send `Syn`, read `Ack`.
#[derive(Debug, Clone, Default)]
pub struct CursorsOutboundInner;

impl HeaderedOutbound for CursorsOutboundInner {
    type Output = Ack;
    type Error = PullsyncError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_CURSORS
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let mut framed = Framed::new(stream.into_inner(), SynCodec::new(MAX_HANDSHAKE_SIZE));
            debug!("Pullsync cursors: sending syn");
            framed.send(Syn).await?;
            let mut framed = reframe(framed, AckCodec::new(MAX_HANDSHAKE_SIZE));
            debug!("Pullsync cursors: reading ack");
            framed
                .try_next()
                .await?
                .ok_or(PullsyncError::ConnectionClosed)
        })
    }
}

// ---------------------------------------------------------------------------
// Range exchange
// ---------------------------------------------------------------------------

/// Sync inbound: read `Get`, return a responder for the offer/want/delivery
/// phases.
#[derive(Debug, Clone)]
pub struct SyncInboundInner;

impl HeaderedInbound for SyncInboundInner {
    type Output = (Get, SyncResponder);
    type Error = PullsyncError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_SYNC
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let mut framed = Framed::new(stream.into_inner(), GetCodec::new(MAX_GET_SIZE));
            debug!("Pullsync sync: reading get");
            let get = framed
                .try_next()
                .await?
                .ok_or(PullsyncError::ConnectionClosed)?;
            debug!(bin = %get.bin, start = get.start, "Pullsync sync: received get");
            let framed = reframe(framed, OfferCodec::new(MAX_OFFER_SIZE));
            Ok((get, SyncResponder::Offering { framed }))
        })
    }
}

/// Server-side driver for the range exchange. The variant encodes the phase, so
/// calling a method out of phase is a compile error.
pub enum SyncResponder {
    /// Awaiting the offer for the requested range.
    Offering {
        framed: Framed<libp2p::Stream, OfferCodec>,
    },
    /// Offer sent; awaiting the want, or finishing if the offer was empty.
    Offered {
        framed: Framed<libp2p::Stream, WantCodec>,
    },
    /// Want received; delivering the wanted chunks.
    Delivering {
        framed: Framed<libp2p::Stream, DeliveryCodec>,
    },
}

impl SyncResponder {
    /// Send the offer and enter the `Offered` phase. An empty offer ends the
    /// exchange with [`finish`](Self::finish) and no want round; a non-empty
    /// offer continues with [`read_want`](Self::read_want).
    pub async fn write_offer(self, offer: Offer) -> Result<Self, PullsyncError> {
        let SyncResponder::Offering { mut framed } = self else {
            return Err(PullsyncError::ConnectionClosed);
        };
        debug!(
            topmost = offer.topmost,
            chunks = offer.chunks.len(),
            "Pullsync sync: sending offer"
        );
        framed.send(offer).await?;
        let framed = reframe(framed, WantCodec::new(MAX_WANT_SIZE));
        Ok(SyncResponder::Offered { framed })
    }

    /// Read the requester's `Want` and enter the delivery phase. Call only after
    /// a non-empty offer; an empty offer is terminated by [`finish`](Self::finish).
    pub async fn read_want(self) -> Result<(Want, Self), PullsyncError> {
        let SyncResponder::Offered { mut framed } = self else {
            return Err(PullsyncError::ConnectionClosed);
        };
        debug!("Pullsync sync: reading want");
        let want = framed
            .try_next()
            .await?
            .ok_or(PullsyncError::ConnectionClosed)?;
        let framed = reframe(framed, DeliveryCodec::new(MAX_DELIVERY_SIZE));
        Ok((want, SyncResponder::Delivering { framed }))
    }

    /// Send one wanted chunk. Call once per set bit in the `Want`, in offer
    /// order.
    pub async fn send_delivery(&mut self, delivery: Delivery) -> Result<(), PullsyncError> {
        let SyncResponder::Delivering { framed } = self else {
            return Err(PullsyncError::ConnectionClosed);
        };
        framed.send(delivery).await
    }

    /// Flush and close the stream, ending the exchange. Valid after an empty
    /// offer (from `Offered`, no want read) or after the last delivery.
    pub async fn finish(self) -> Result<(), PullsyncError> {
        match self {
            SyncResponder::Offered { mut framed } => {
                framed.close().await?;
                Ok(())
            }
            SyncResponder::Delivering { mut framed } => {
                framed.close().await?;
                Ok(())
            }
            SyncResponder::Offering { .. } => Err(PullsyncError::ConnectionClosed),
        }
    }
}

/// Sync outbound: send `Get`, read `Offer`, return a driver for the want and
/// delivery phases.
#[derive(Debug, Clone)]
pub struct SyncOutboundInner {
    get: Get,
}

impl SyncOutboundInner {
    pub fn new(get: Get) -> Self {
        Self { get }
    }
}

impl HeaderedOutbound for SyncOutboundInner {
    type Output = (Offer, SyncRequester);
    type Error = PullsyncError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_SYNC
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let mut framed = Framed::new(stream.into_inner(), GetCodec::new(MAX_GET_SIZE));
            debug!(bin = %self.get.bin, start = self.get.start, "Pullsync sync: sending get");
            framed.send(self.get).await?;
            let mut framed = reframe(framed, OfferCodec::new(MAX_OFFER_SIZE));
            debug!("Pullsync sync: reading offer");
            let offer = framed
                .try_next()
                .await?
                .ok_or(PullsyncError::ConnectionClosed)?;
            let framed = reframe(framed, WantCodec::new(MAX_WANT_SIZE));
            Ok((offer, SyncRequester::Wanting { framed }))
        })
    }
}

/// Client-side driver for the range exchange after the offer arrives. After
/// [`send_want`](Self::send_want) the caller reads exactly
/// [`Want::count`](crate::Want::count) deliveries. An empty offer is terminated
/// by [`finish`](Self::finish) with no want.
pub enum SyncRequester {
    /// Offer received; awaiting the want to send, or finishing if empty.
    Wanting {
        framed: Framed<libp2p::Stream, WantCodec>,
    },
    /// Want sent; receiving the selected deliveries.
    Receiving {
        framed: Framed<libp2p::Stream, DeliveryCodec>,
    },
}

impl SyncRequester {
    /// Send the selection and enter the receive phase. Call only for a non-empty
    /// offer; an empty offer is terminated by [`finish`](Self::finish).
    pub async fn send_want(self, want: Want) -> Result<Self, PullsyncError> {
        let SyncRequester::Wanting { mut framed } = self else {
            return Err(PullsyncError::ConnectionClosed);
        };
        debug!(wanted = want.count(), "Pullsync sync: sending want");
        framed.send(want).await?;
        let framed = reframe(framed, DeliveryCodec::new(MAX_DELIVERY_SIZE));
        Ok(SyncRequester::Receiving { framed })
    }

    /// Read the next delivery, or `None` when the responder closes the stream.
    pub async fn next_delivery(&mut self) -> Result<Option<Delivery>, PullsyncError> {
        let SyncRequester::Receiving { framed } = self else {
            return Err(PullsyncError::ConnectionClosed);
        };
        framed.try_next().await
    }

    /// Close the stream from the `Wanting` phase, ending the exchange with no
    /// want. Used when the offer was empty.
    pub async fn finish(self) -> Result<(), PullsyncError> {
        let SyncRequester::Wanting { mut framed } = self else {
            return Err(PullsyncError::ConnectionClosed);
        };
        framed.close().await?;
        Ok(())
    }
}

pub type CursorsInboundProtocol = Inbound<CursorsInboundInner>;
pub type CursorsOutboundProtocol = Outbound<CursorsOutboundInner>;
pub type SyncInboundProtocol = Inbound<SyncInboundInner>;
pub type SyncOutboundProtocol = Outbound<SyncOutboundInner>;

pub fn cursors_inbound() -> CursorsInboundProtocol {
    Inbound::new(CursorsInboundInner)
}

pub fn cursors_outbound() -> CursorsOutboundProtocol {
    Outbound::new(CursorsOutboundInner)
}

pub fn sync_inbound() -> SyncInboundProtocol {
    Inbound::new(SyncInboundInner)
}

pub fn sync_outbound(get: Get) -> SyncOutboundProtocol {
    Outbound::new(SyncOutboundInner::new(get))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;
    use nectar_primitives::ChunkAddress;

    use crate::ChunkDescriptor;

    use super::*;

    // The empty-offer flow keys on `Offer::chunks.is_empty()`: an empty offer is
    // finished with no want on both sides, a non-empty offer proceeds to the want
    // round. The stream drivers wrap an opaque `libp2p::Stream` and so cannot be
    // exercised without a live transport; the phase-typed enums make the
    // sequencing a compile-time contract, and the gate below is the runtime seam
    // the behaviour layer branches on.
    #[test]
    fn empty_offer_is_the_no_want_gate() {
        let empty = Offer::new(7, vec![]);
        assert!(empty.chunks.is_empty(), "empty offer ends with no want");

        let one = Offer::new(
            7,
            vec![ChunkDescriptor::new(
                ChunkAddress::new([0; 32]),
                B256::ZERO,
                B256::ZERO,
            )],
        );
        assert!(!one.chunks.is_empty(), "non-empty offer proceeds to want");
    }
}
