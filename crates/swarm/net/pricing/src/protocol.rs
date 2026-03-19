//! Protocol upgrade for pricing.
//!
//! Implements HeaderedInbound/HeaderedOutbound traits - headers are automatic.

use asynchronous_codec::Framed;
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use tracing::debug;
use vertex_swarm_net_headers::{
    HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound,
};

use crate::{
    PROTOCOL_NAME,
    codec::{AnnouncePaymentThreshold, PricingCodec},
    error::PricingError,
};

/// Maximum size of a pricing message.
const MAX_MESSAGE_SIZE: usize = 1024;

/// Pricing inbound: receives threshold from remote.
#[derive(Debug, Clone, Default)]
pub struct PricingInner;

impl HeaderedInbound for PricingInner {
    type Output = AnnouncePaymentThreshold;
    type Error = PricingError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let codec = PricingCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Pricing: Reading peer threshold");
            framed
                .try_next()
                .await?
                .ok_or(PricingError::ConnectionClosed)
        })
    }
}

/// Pricing outbound: sends threshold to remote.
#[derive(Debug, Clone)]
pub struct PricingOutboundInner {
    threshold: AnnouncePaymentThreshold,
}

impl PricingOutboundInner {
    pub fn new(threshold: AnnouncePaymentThreshold) -> Self {
        Self { threshold }
    }
}

impl HeaderedOutbound for PricingOutboundInner {
    type Output = ();
    type Error = PricingError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let codec = PricingCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Pricing: Sending our threshold");
            framed.send(self.threshold).await?;
            Ok(())
        })
    }
}

// Type aliases for handler
pub type PricingInboundProtocol = Inbound<PricingInner>;
pub type PricingOutboundProtocol = Outbound<PricingOutboundInner>;

pub fn inbound() -> PricingInboundProtocol {
    Inbound::new(PricingInner)
}

pub fn outbound(threshold: AnnouncePaymentThreshold) -> PricingOutboundProtocol {
    Outbound::new(PricingOutboundInner::new(threshold))
}
