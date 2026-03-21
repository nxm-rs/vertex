//! Protocol upgrade for credit limit announcement.
//!
//! Implements HeaderedInbound/HeaderedOutbound traits — headers are automatic.

use asynchronous_codec::Framed;
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use tracing::debug;
use vertex_swarm_net_headers::{
    HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound,
};

use crate::{
    PROTOCOL_NAME,
    codec::{AnnounceCreditLimit, CreditCodec},
    error::CreditError,
};

/// Maximum size of a credit protocol message.
const MAX_MESSAGE_SIZE: usize = 1024;

/// Credit inbound: receives credit limit from remote.
#[derive(Debug, Clone, Default)]
pub struct CreditInner;

impl HeaderedInbound for CreditInner {
    type Output = AnnounceCreditLimit;
    type Error = CreditError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let codec = CreditCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Credit: Reading peer credit limit");
            framed
                .try_next()
                .await?
                .ok_or(CreditError::ConnectionClosed)
        })
    }
}

/// Credit outbound: sends credit limit to remote.
#[derive(Debug, Clone)]
pub struct CreditOutboundInner {
    limit: AnnounceCreditLimit,
}

impl CreditOutboundInner {
    pub fn new(limit: AnnounceCreditLimit) -> Self {
        Self { limit }
    }
}

impl HeaderedOutbound for CreditOutboundInner {
    type Output = ();
    type Error = CreditError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let codec = CreditCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Credit: Sending our credit limit");
            framed.send(self.limit).await?;
            Ok(())
        })
    }
}

/// Headered inbound protocol for receiving a credit limit announcement.
pub type CreditInboundProtocol = Inbound<CreditInner>;
/// Headered outbound protocol for sending a credit limit announcement.
pub type CreditOutboundProtocol = Outbound<CreditOutboundInner>;

/// Create an inbound credit protocol upgrade.
pub fn inbound() -> CreditInboundProtocol {
    Inbound::new(CreditInner)
}

/// Create an outbound credit protocol upgrade that sends the given limit.
pub fn outbound(limit: AnnounceCreditLimit) -> CreditOutboundProtocol {
    Outbound::new(CreditOutboundInner::new(limit))
}
