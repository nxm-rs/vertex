use futures::future::BoxFuture;
use libp2p::{core::UpgradeInfo, InboundUpgrade, OutboundUpgrade, Stream};

use crate::{HandshakeConfig, HandshakeError};

#[derive(Debug, Clone)]
pub struct HandshakeProtocol<const N: u64> {
    pub(crate) config: HandshakeConfig<N>,
}

impl<const N: u64> UpgradeInfo for HandshakeProtocol<N> {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once("/swarm/handshake/13.0.0/handshake")
    }
}

impl<const N: u64> InboundUpgrade<Stream> for HandshakeProtocol<N> {
    type Output = Stream;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        // Just return the negotiated stream
        Box::pin(futures::future::ok(socket))
    }
}

impl<const N: u64> OutboundUpgrade<Stream> for HandshakeProtocol<N> {
    type Output = Stream;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        // Just return the negotiated stream
        Box::pin(futures::future::ok(socket))
    }
}
