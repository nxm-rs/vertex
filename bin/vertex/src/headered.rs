use futures::io::{AsyncReadExt, AsyncWriteExt};
use futures::{AsyncRead, AsyncWrite, Future};
use libp2p::core::{
    upgrade::{InboundUpgrade, OutboundUpgrade, UpgradeInfo},
    Negotiated,
};
use libp2p::swarm::StreamProtocol;
use std::pin::Pin; // Add these traits

/// A wrapper around a [`StreamProtocol`] that handles a header exchange between peers.
#[derive(Debug, Clone)]
pub struct HeaderedProtocol<H> {
    inner: StreamProtocol,
    header: H,
}

impl<H> HeaderedProtocol<H>
where
    H: AsRef<[u8]> + Clone + Send + 'static,
{
    /// Create a new [`HeaderedProtocol`] with the given protocol and header.
    pub fn new(protocol: impl Into<StreamProtocol>, header: H) -> Self {
        Self {
            inner: protocol.into(),
            header,
        }
    }

    /// Get the underlying protocol.
    pub fn into_inner(self) -> StreamProtocol {
        self.inner
    }
}

impl<H> UpgradeInfo for HeaderedProtocol<H>
where
    H: AsRef<[u8]> + Clone + Send + 'static,
{
    type Info = StreamProtocol;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(self.inner.clone())
    }
}

impl<H, T> InboundUpgrade<Negotiated<T>> for HeaderedProtocol<H>
where
    H: AsRef<[u8]> + Clone + Send + 'static,
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Output = Negotiated<T>;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_inbound(self, mut socket: Negotiated<T>, _: Self::Info) -> Self::Future {
        Box::pin(async move {
            // Read peer's header
            let mut header_buf = vec![0u8; self.header.as_ref().len()];
            AsyncReadExt::read_exact(&mut socket, &mut header_buf).await?;

            // Write our header
            AsyncWriteExt::write_all(&mut socket, self.header.as_ref()).await?;
            AsyncWriteExt::flush(&mut socket).await?;

            Ok(socket)
        })
    }
}

impl<H, T> OutboundUpgrade<Negotiated<T>> for HeaderedProtocol<H>
where
    H: AsRef<[u8]> + Clone + Send + 'static,
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Output = Negotiated<T>;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_outbound(self, mut socket: Negotiated<T>, _: Self::Info) -> Self::Future {
        Box::pin(async move {
            // Write our header
            AsyncWriteExt::write_all(&mut socket, self.header.as_ref()).await?;
            AsyncWriteExt::flush(&mut socket).await?;

            // Read peer's header
            let mut header_buf = vec![0u8; self.header.as_ref().len()];
            AsyncReadExt::read_exact(&mut socket, &mut header_buf).await?;

            Ok(socket)
        })
    }
}
