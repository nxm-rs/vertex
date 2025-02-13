use bytes::Bytes;
use futures::{AsyncRead, AsyncWrite, Future, StreamExt};
use libp2p::core::{
    upgrade::{InboundUpgrade, OutboundUpgrade, UpgradeInfo},
    Negotiated,
};
use libp2p::swarm::StreamProtocol;
use quick_protobuf::{MessageRead, MessageWrite, Writer};
use std::{collections::HashMap, pin::Pin};
use tracing::{info, info_span, Instrument};

use crate::proto::headers::{Header, Headers};

#[derive(Debug, Clone)]
pub struct ProtocolHeaders {
    headers: HashMap<String, Bytes>,
}

impl ProtocolHeaders {
    pub fn new() -> Self {
        Self {
            headers: HashMap::new(),
        }
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Bytes>) -> &mut Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    pub fn get(&self, key: &str) -> Option<&Bytes> {
        self.headers.get(key)
    }

    fn to_proto(&self) -> Headers {
        Headers {
            headers: self
                .headers
                .iter()
                .map(|(k, v)| Header {
                    key: k.clone(),
                    value: v.to_vec(),
                })
                .collect(),
        }
    }

    fn from_proto(proto: Headers) -> Self {
        let mut headers = HashMap::new();
        for kv in proto.headers {
            headers.insert(kv.key, kv.value.into());
        }
        Self { headers }
    }
}

#[derive(Debug, Clone)]
pub struct HeaderedProtocol<P> {
    inner: P,
    headers: ProtocolHeaders,
}

impl<P> HeaderedProtocol<P> {
    pub fn new(inner: P, headers: ProtocolHeaders) -> Self {
        Self { inner, headers }
    }

    pub fn headers(&self) -> &ProtocolHeaders {
        &self.headers
    }

    pub fn into_inner(self) -> P {
        self.inner
    }
}

impl<P: UpgradeInfo> UpgradeInfo for HeaderedProtocol<P> {
    type Info = P::Info;
    type InfoIter = P::InfoIter;

    fn protocol_info(&self) -> Self::InfoIter {
        self.inner.protocol_info()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("Protobuf error: {0}")]
    Protobuf(#[from] quick_protobuf::Error),
}

async fn write_headers<T: AsyncWrite + Unpin>(
    socket: &mut T,
    headers: &ProtocolHeaders,
) -> Result<(), HeaderError> {
    let proto = headers.to_proto();
    let mut buf = Vec::new();
    let mut writer = Writer::new(&mut buf);
    proto
        .write_message(&mut writer)
        .map_err(HeaderError::from)?;

    // Write length prefix
    let len = buf.len() as u16;
    socket
        .write_all(&len.to_be_bytes())
        .await
        .map_err(HeaderError::from)?;

    // Write serialized headers
    socket.write_all(&buf).await.map_err(HeaderError::from)?;
    socket.flush().await.map_err(HeaderError::from)?;

    Ok(())
}

async fn read_headers<T: AsyncRead + Unpin>(
    socket: &mut T,
) -> Result<ProtocolHeaders, HeaderError> {
    // Read length prefix
    let mut len_buf = [0u8; 2];
    socket
        .read_exact(&mut len_buf)
        .await
        .map_err(HeaderError::from)?;
    let len = u16::from_be_bytes(len_buf) as usize;

    // Read header data
    let mut buf = vec![0u8; len];
    socket
        .read_exact(&mut buf)
        .await
        .map_err(HeaderError::from)?;

    // Parse headers
    let proto = Headers::from_reader(&mut &buf[..]).map_err(HeaderError::from)?;
    Ok(ProtocolHeaders::from_proto(proto))
}

impl<P, T> InboundUpgrade<Negotiated<T>> for HeaderedProtocol<P>
where
    P: InboundUpgrade<Negotiated<T>>,
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Output = P::Output;
    type Error = HeaderError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_inbound(self, mut socket: Negotiated<T>, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            // Read peer's headers
            let peer_headers = read_headers(&mut socket).await?;

            // Create tracing span with received headers
            let span = info_span!(
                "inbound_protocol",
                peer_headers = ?peer_headers.headers
            );

            // Write our headers
            write_headers(&mut socket, &self.headers).await?;

            // Upgrade inner protocol within tracing span
            self.inner
                .upgrade_inbound(socket, info)
                .instrument(span)
                .await
                .map_err(|e| HeaderError::Protocol(e.to_string()))
        })
    }
}

impl<P, T> OutboundUpgrade<Negotiated<T>> for HeaderedProtocol<P>
where
    P: OutboundUpgrade<Negotiated<T>>,
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Output = P::Output;
    type Error = HeaderError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_outbound(self, mut socket: Negotiated<T>, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            // Write our headers
            write_headers(&mut socket, &self.headers).await?;

            // Read peer's headers
            let peer_headers = read_headers(&mut socket).await?;

            // Create tracing span with both headers
            let span = info_span!(
                "outbound_protocol",
                local_headers = ?self.headers.headers,
                peer_headers = ?peer_headers.headers
            );

            // Upgrade inner protocol within tracing span
            self.inner
                .upgrade_outbound(socket, info)
                .instrument(span)
                .await
                .map_err(|e| HeaderError::Protocol(e.to_string()))
        })
    }
}
