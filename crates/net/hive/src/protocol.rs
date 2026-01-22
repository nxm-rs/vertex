//! Protocol upgrade for hive.
//!
//! The hive protocol is a unidirectional peer gossip protocol with headers.
//! - Outbound: Headers exchange, then send a `Peers` message
//! - Inbound: Headers exchange, then receive a `Peers` message
//!
//! There is no request/response pattern for the peers data - messages flow one way.

use std::collections::HashMap;

use asynchronous_codec::Framed;
use bytes::Bytes;
use futures::{future::BoxFuture, SinkExt, TryStreamExt};
use libp2p::{core::UpgradeInfo, InboundUpgrade, OutboundUpgrade, Stream};
use tracing::debug;
use vertex_net_headers::{CodecError as HeadersCodecError, Headers, HeadersCodec};

use crate::{
    codec::{HiveCodec, HiveCodecError, Peers},
    PROTOCOL_NAME,
};

/// Maximum size of a hive message (accommodates ~30 peers with multiple underlays).
const MAX_MESSAGE_SIZE: usize = 32 * 1024; // 32 KB

/// Maximum size of headers message.
const MAX_HEADERS_SIZE: usize = 1024;

/// Hive protocol upgrade for receiving peers.
///
/// Inbound connections receive a `Peers` message from the remote.
#[derive(Debug, Clone, Default)]
pub struct HiveInboundProtocol {
    /// Headers to send in response.
    headers: HashMap<String, Bytes>,
}

impl HiveInboundProtocol {
    /// Create a new inbound protocol handler.
    pub fn new() -> Self {
        Self {
            headers: HashMap::new(),
        }
    }
}

/// Output of a successful inbound hive protocol upgrade.
#[derive(Debug)]
pub struct HiveInboundOutput {
    /// The peers received from the remote.
    pub peers: Peers,
    /// Headers received from the peer.
    pub headers: HashMap<String, Bytes>,
    /// The underlying stream (for closing/cleanup).
    pub stream: Stream,
}

/// Error during hive protocol upgrade.
#[derive(Debug, thiserror::Error)]
pub enum HiveError {
    /// Headers exchange failed.
    #[error("Headers error: {0}")]
    Headers(#[from] HeadersCodecError),
    /// Codec error.
    #[error("Codec error: {0}")]
    Codec(#[from] HiveCodecError),
    /// Connection was closed before completion.
    #[error("Connection closed")]
    ConnectionClosed,
}

impl UpgradeInfo for HiveInboundProtocol {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL_NAME)
    }
}

impl InboundUpgrade<Stream> for HiveInboundProtocol {
    type Output = HiveInboundOutput;
    type Error = HiveError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, _info: Self::Info) -> Self::Future {
        Box::pin(async move {
            // Phase 1: Headers exchange
            // Inbound: read peer's headers first, then send our response
            let headers_codec = HeadersCodec::new(MAX_HEADERS_SIZE);
            let mut framed = Framed::new(socket, headers_codec);

            debug!("Hive: Reading peer headers");
            let peer_headers = framed
                .try_next()
                .await?
                .ok_or(HiveError::ConnectionClosed)?
                .into_inner();

            debug!("Hive: Sending our headers");
            framed.send(Headers::new(self.headers)).await?;

            let stream = framed.into_inner();

            // Phase 2: Read the Peers message
            let codec: HiveCodec = HiveCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream, codec);

            debug!("Hive: Reading peers message");
            let peers = framed
                .try_next()
                .await?
                .ok_or(HiveError::ConnectionClosed)?;

            let stream = framed.into_inner();

            Ok(HiveInboundOutput {
                peers,
                headers: peer_headers,
                stream,
            })
        })
    }
}

/// Hive protocol upgrade for sending peers.
///
/// Outbound connections send a `Peers` message to the remote.
#[derive(Debug, Clone)]
pub struct HiveOutboundProtocol {
    /// The peers to send.
    peers: Peers,
    /// Headers to send.
    headers: HashMap<String, Bytes>,
}

impl HiveOutboundProtocol {
    /// Create a new outbound protocol handler with the peers to send.
    pub fn new(peers: Peers) -> Self {
        Self {
            peers,
            headers: HashMap::new(),
        }
    }
}

/// Output of a successful outbound hive protocol upgrade.
#[derive(Debug)]
pub struct HiveOutboundOutput {
    /// Headers received from the peer.
    pub headers: HashMap<String, Bytes>,
    /// The underlying stream (for closing/cleanup).
    pub stream: Stream,
}

impl UpgradeInfo for HiveOutboundProtocol {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL_NAME)
    }
}

impl OutboundUpgrade<Stream> for HiveOutboundProtocol {
    type Output = HiveOutboundOutput;
    type Error = HiveError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, _info: Self::Info) -> Self::Future {
        Box::pin(async move {
            // Phase 1: Headers exchange
            // Outbound: send our headers first, then read response
            let headers_codec = HeadersCodec::new(MAX_HEADERS_SIZE);
            let mut framed = Framed::new(socket, headers_codec);

            debug!("Hive: Sending our headers");
            framed.send(Headers::new(self.headers)).await?;

            debug!("Hive: Reading peer headers");
            let peer_headers = framed
                .try_next()
                .await?
                .ok_or(HiveError::ConnectionClosed)?
                .into_inner();

            let stream = framed.into_inner();

            // Phase 2: Send the Peers message
            let codec: HiveCodec = HiveCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream, codec);

            debug!("Hive: Sending peers message");
            framed.send(self.peers).await?;

            let stream = framed.into_inner();

            Ok(HiveOutboundOutput {
                headers: peer_headers,
                stream,
            })
        })
    }
}
