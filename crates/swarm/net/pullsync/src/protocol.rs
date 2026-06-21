//! Protocol upgrades for pullsync's two streams.
//!
//! Both sub-protocols are headered (the headers exchange is automatic):
//!
//! - **Cursors** ([`PROTOCOL_CURSORS`]): one round. Outbound sends `Syn` and
//!   reads `Ack`; inbound reads `Syn` and returns a responder that sends `Ack`.
//! - **Sync** ([`PROTOCOL_SYNC`]): four phases on one stream. Outbound sends
//!   `Get`, reads `Offer`, sends `Want`, then reads one `Delivery` per set bit.
//!   Inbound reads `Get` and returns a responder driven by the behaviour layer:
//!   `send_offer`, `recv_want`, `send_delivery` per wanted chunk, then `finish`.
//!   Each phase switches the codec on the same stream via `into_parts()`, which
//!   preserves any bytes already buffered.

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
/// `ceil(DEFAULT_MAX_PAGE / 8)` bytes.
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

/// Server-side driver for the range exchange, advanced by the behaviour layer.
///
/// The type encodes the phase: `Offering` accepts [`send_offer`](Self::send_offer);
/// `Delivering` accepts [`send_delivery`](Self::send_delivery) and
/// [`finish`](Self::finish). Calling out of phase is a compile error.
pub enum SyncResponder {
    /// Awaiting the offer for the requested range.
    Offering {
        framed: Framed<libp2p::Stream, OfferCodec>,
    },
    /// Offer sent and want received; delivering the wanted chunks.
    Delivering {
        framed: Framed<libp2p::Stream, DeliveryCodec>,
    },
}

impl SyncResponder {
    /// Send the offer, read the requester's `Want`, and enter the delivery
    /// phase. Returns the want so the caller knows which descriptors to deliver.
    pub async fn send_offer(self, offer: Offer) -> Result<(Want, Self), PullsyncError> {
        let SyncResponder::Offering { mut framed } = self else {
            return Err(PullsyncError::ConnectionClosed);
        };
        debug!(
            topmost = offer.topmost,
            chunks = offer.chunks.len(),
            "Pullsync sync: sending offer"
        );
        framed.send(offer).await?;
        let mut framed = reframe(framed, WantCodec::new(MAX_WANT_SIZE));
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

    /// Flush and close the delivery stream after the last `send_delivery`.
    pub async fn finish(self) -> Result<(), PullsyncError> {
        let SyncResponder::Delivering { mut framed } = self else {
            return Err(PullsyncError::ConnectionClosed);
        };
        framed.close().await?;
        Ok(())
    }
}

/// Sync outbound: send `Get`, read `Offer`, return a driver for the want and
/// delivery phases.
#[derive(Debug, Clone)]
pub struct SyncOutboundInner {
    get: Get,
}

impl SyncOutboundInner {
    /// Create an outbound range exchange for the given `Get`.
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

/// Client-side driver for the range exchange after the offer arrives.
///
/// `accept_want` sends the selection and moves to receiving the deliveries; the
/// caller then reads exactly [`Want::count`](crate::Want::count) deliveries.
pub enum SyncRequester {
    /// Offer received; awaiting the want to send.
    Wanting {
        framed: Framed<libp2p::Stream, WantCodec>,
    },
    /// Want sent; receiving the selected deliveries.
    Receiving {
        framed: Framed<libp2p::Stream, DeliveryCodec>,
    },
}

impl SyncRequester {
    /// Send the selection and enter the receive phase.
    pub async fn accept_want(self, want: Want) -> Result<Self, PullsyncError> {
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
}

/// Inbound cursor-handshake protocol type for the handler.
pub type CursorsInboundProtocol = Inbound<CursorsInboundInner>;
/// Outbound cursor-handshake protocol type for the handler.
pub type CursorsOutboundProtocol = Outbound<CursorsOutboundInner>;
/// Inbound range-exchange protocol type for the handler.
pub type SyncInboundProtocol = Inbound<SyncInboundInner>;
/// Outbound range-exchange protocol type for the handler.
pub type SyncOutboundProtocol = Outbound<SyncOutboundInner>;

/// Create an inbound cursor-handshake handler.
pub fn cursors_inbound() -> CursorsInboundProtocol {
    Inbound::new(CursorsInboundInner)
}

/// Create an outbound cursor-handshake handler.
pub fn cursors_outbound() -> CursorsOutboundProtocol {
    Outbound::new(CursorsOutboundInner)
}

/// Create an inbound range-exchange handler.
pub fn sync_inbound() -> SyncInboundProtocol {
    Inbound::new(SyncInboundInner)
}

/// Create an outbound range-exchange handler for the given `Get`.
pub fn sync_outbound(get: Get) -> SyncOutboundProtocol {
    Outbound::new(SyncOutboundInner::new(get))
}
