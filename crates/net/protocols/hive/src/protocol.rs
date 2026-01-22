//! Protocol upgrade for hive.
//!
//! Implements HeaderedInbound/HeaderedOutbound traits - headers are automatic.

use asynchronous_codec::Framed;
use futures::{future::BoxFuture, SinkExt, TryStreamExt};
use tracing::debug;
use vertex_net_headers::{HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound};

use crate::{
    codec::{HiveCodec, HiveCodecError, Peers},
    PROTOCOL_NAME,
};

const MAX_MESSAGE_SIZE: usize = 32 * 1024;

/// Hive inbound: receives peers from remote.
#[derive(Debug, Clone, Default)]
pub struct HiveInner;

impl HeaderedInbound for HiveInner {
    type Output = Peers;
    type Error = HiveCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let codec = HiveCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Hive: Reading peers message");
            framed
                .try_next()
                .await?
                .ok_or_else(|| HiveCodecError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed",
                )))
        })
    }
}

/// Hive outbound: sends peers to remote.
#[derive(Debug, Clone)]
pub struct HiveOutboundInner {
    peers: Peers,
}

impl HiveOutboundInner {
    pub fn new(peers: Peers) -> Self {
        Self { peers }
    }
}

impl HeaderedOutbound for HiveOutboundInner {
    type Output = ();
    type Error = HiveCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn write(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let codec = HiveCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Hive: Sending peers message");
            framed.send(self.peers).await?;
            Ok(())
        })
    }
}

// Type aliases for handler
pub type HiveInboundProtocol = Inbound<HiveInner>;
pub type HiveOutboundProtocol = Outbound<HiveOutboundInner>;

pub fn inbound() -> HiveInboundProtocol {
    Inbound::new(HiveInner)
}

pub fn outbound(peers: Peers) -> HiveOutboundProtocol {
    Outbound::new(HiveOutboundInner::new(peers))
}
