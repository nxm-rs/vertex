//! Protocol upgrade for pricing.
//!
//! The pricing protocol uses headers like other Bee protocols.
//! Each stream is UNIDIRECTIONAL:
//! - Outbound: Send our threshold to the peer
//! - Inbound: Receive the peer's threshold
//!
//! When we receive a peer's threshold (inbound), we should also send
//! our threshold to them via a NEW outbound stream.

use std::collections::HashMap;

use asynchronous_codec::Framed;
use bytes::Bytes;
use futures::{future::BoxFuture, SinkExt, TryStreamExt};
use libp2p::{core::UpgradeInfo, InboundUpgrade, OutboundUpgrade, Stream};
use tracing::debug;

use crate::{
    codec::{AnnouncePaymentThreshold, PricingCodec, PricingCodecError},
    PROTOCOL_NAME,
};

// Import headers types
use vertex_net_headers::{CodecError as HeadersCodecError, Headers, HeadersCodec};

/// Maximum size of a pricing message.
const MAX_MESSAGE_SIZE: usize = 1024;

/// Maximum size of headers message.
const MAX_HEADERS_SIZE: usize = 1024;

/// Pricing protocol upgrade.
///
/// Handles the protocol exchange:
/// 1. Headers exchange (bidirectional)
/// 2. Threshold send (outbound) or receive (inbound) - UNIDIRECTIONAL
#[derive(Debug, Clone)]
pub struct PricingProtocol {
    /// The payment threshold to announce.
    threshold: AnnouncePaymentThreshold,
    /// Headers to send (typically empty for pricing).
    headers: HashMap<String, Bytes>,
}

impl PricingProtocol {
    /// Create a new pricing protocol with the given threshold.
    pub fn new(threshold: AnnouncePaymentThreshold) -> Self {
        Self {
            threshold,
            headers: HashMap::new(),
        }
    }

    /// Create with custom headers (for tracing, etc.).
    pub fn with_headers(
        threshold: AnnouncePaymentThreshold,
        headers: HashMap<String, Bytes>,
    ) -> Self {
        Self { threshold, headers }
    }
}

/// Output of a successful inbound pricing protocol upgrade.
#[derive(Debug)]
pub struct PricingInboundOutput {
    /// The underlying stream (after headers and pricing exchange).
    pub stream: Stream,
    /// Headers received from the peer.
    pub headers: HashMap<String, Bytes>,
    /// The payment threshold announced by the peer.
    pub peer_threshold: AnnouncePaymentThreshold,
}

/// Output of a successful outbound pricing protocol upgrade.
#[derive(Debug)]
pub struct PricingOutboundOutput {
    /// The underlying stream (after headers and pricing exchange).
    pub stream: Stream,
    /// Headers received from the peer.
    pub headers: HashMap<String, Bytes>,
}

/// Error during pricing protocol upgrade.
#[derive(Debug, thiserror::Error)]
pub enum PricingError {
    /// Headers exchange failed.
    #[error("Headers error: {0}")]
    Headers(#[from] HeadersCodecError),
    /// Pricing codec error.
    #[error("Codec error: {0}")]
    Codec(#[from] PricingCodecError),
    /// Connection was closed before completion.
    #[error("Connection closed")]
    ConnectionClosed,
    /// Peer's threshold is below minimum.
    #[error("Threshold too low: {threshold}, minimum is {minimum}")]
    ThresholdTooLow { threshold: u64, minimum: u64 },
}

impl UpgradeInfo for PricingProtocol {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL_NAME)
    }
}

impl InboundUpgrade<Stream> for PricingProtocol {
    type Output = PricingInboundOutput;
    type Error = PricingError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, _info: Self::Info) -> Self::Future {
        Box::pin(async move {
            // Phase 1: Headers exchange
            // Inbound: read peer's headers first, then send our response
            let headers_codec = HeadersCodec::new(MAX_HEADERS_SIZE);
            let mut framed = Framed::new(socket, headers_codec);

            debug!("Reading peer headers");
            let peer_headers = framed
                .try_next()
                .await?
                .ok_or(PricingError::ConnectionClosed)?
                .into_inner();

            debug!("Sending our headers");
            framed.send(Headers::new(self.headers)).await?;

            let stream = framed.into_inner();

            // Phase 2: Pricing - INBOUND ONLY READS
            // Read peer's threshold, do NOT send ours (that happens on a separate stream)
            let pricing_codec: PricingCodec = PricingCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream, pricing_codec);

            debug!("Reading peer threshold");
            let peer_threshold: AnnouncePaymentThreshold = framed
                .try_next()
                .await?
                .ok_or(PricingError::ConnectionClosed)?;

            let stream = framed.into_inner();

            Ok(PricingInboundOutput {
                stream,
                headers: peer_headers,
                peer_threshold,
            })
        })
    }
}

impl OutboundUpgrade<Stream> for PricingProtocol {
    type Output = PricingOutboundOutput;
    type Error = PricingError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, _info: Self::Info) -> Self::Future {
        Box::pin(async move {
            // Phase 1: Headers exchange
            // Outbound: send our headers first, then read response
            let headers_codec = HeadersCodec::new(MAX_HEADERS_SIZE);
            let mut framed = Framed::new(socket, headers_codec);

            debug!("Sending our headers");
            framed.send(Headers::new(self.headers)).await?;

            debug!("Reading peer headers");
            let peer_headers = framed
                .try_next()
                .await?
                .ok_or(PricingError::ConnectionClosed)?
                .into_inner();

            let stream = framed.into_inner();

            // Phase 2: Pricing - OUTBOUND ONLY WRITES
            // Send our threshold, do NOT read (peer sends theirs on a separate stream)
            let pricing_codec: PricingCodec = PricingCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream, pricing_codec);

            debug!("Sending our threshold");
            framed.send(self.threshold).await?;

            let stream = framed.into_inner();

            Ok(PricingOutboundOutput {
                stream,
                headers: peer_headers,
            })
        })
    }
}
