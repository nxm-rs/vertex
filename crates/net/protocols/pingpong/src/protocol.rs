//! Protocol upgrade for pingpong.
//!
//! Implements HeaderedInbound/HeaderedOutbound traits - headers are automatic.
//!
//! # Protocol Flow
//!
//! Pingpong is a request/response protocol:
//! - **Outbound (pinger)**: Send Ping, receive Pong
//! - **Inbound (ponger)**: Receive Ping, send Pong

use asynchronous_codec::Framed;
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use tracing::debug;
use vertex_net_headers::{HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound};

use crate::{
    PROTOCOL_NAME,
    codec::{Ping, PingCodec, PingpongCodecError, Pong, PongCodec},
};

/// Maximum size of a pingpong message.
const MAX_MESSAGE_SIZE: usize = 4096;

// ============================================================================
// Inbound (Ponger) - Receives ping, sends pong
// ============================================================================

/// Pingpong inbound: receives a ping from remote, sends pong response.
#[derive(Debug, Clone)]
pub struct PingpongInboundInner;

impl HeaderedInbound for PingpongInboundInner {
    type Output = ();
    type Error = PingpongCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let codec = PingCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Pingpong: Reading ping");
            let ping = framed.try_next().await?.ok_or_else(|| {
                PingpongCodecError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed",
                ))
            })?;

            debug!(greeting = %ping.greeting, "Pingpong: Received ping");

            // Send pong response (wrap greeting in braces like Bee)
            let pong_codec = PongCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(framed.into_inner(), pong_codec);
            let pong = Pong::from_greeting(&ping.greeting);

            debug!(response = %pong.response, "Pingpong: Sending pong");
            framed.send(pong).await?;

            Ok(())
        })
    }
}

// ============================================================================
// Outbound (Pinger) - Sends ping, receives pong
// ============================================================================

/// Pingpong outbound: sends a ping, receives pong response.
#[derive(Debug, Clone)]
pub struct PingpongOutboundInner {
    greeting: String,
}

impl PingpongOutboundInner {
    /// Create a new outbound ping with the given greeting.
    pub fn new(greeting: impl Into<String>) -> Self {
        Self {
            greeting: greeting.into(),
        }
    }
}

impl HeaderedOutbound for PingpongOutboundInner {
    type Output = Pong;
    type Error = PingpongCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            // Send the ping
            let ping_codec = PingCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), ping_codec);

            debug!(greeting = %self.greeting, "Pingpong: Sending ping");
            framed.send(Ping::new(&self.greeting)).await?;

            // Switch to pong codec and read response
            let pong_codec = PongCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(framed.into_inner(), pong_codec);

            debug!("Pingpong: Reading pong response");
            framed.try_next().await?.ok_or_else(|| {
                PingpongCodecError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed",
                ))
            })
        })
    }
}

// ============================================================================
// Type Aliases and Constructors
// ============================================================================

/// Inbound protocol type for handler.
pub type PingpongInboundProtocol = Inbound<PingpongInboundInner>;

/// Outbound protocol type for handler.
pub type PingpongOutboundProtocol = Outbound<PingpongOutboundInner>;

/// Create an inbound protocol handler.
pub fn inbound() -> PingpongInboundProtocol {
    Inbound::new(PingpongInboundInner)
}

/// Create an outbound protocol handler with the given greeting.
pub fn outbound(greeting: impl Into<String>) -> PingpongOutboundProtocol {
    Outbound::new(PingpongOutboundInner::new(greeting))
}
