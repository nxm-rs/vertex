//! Protocol upgrade for pingpong.

use futures::future::BoxFuture;
use metrics::histogram;
use tracing::debug;
use vertex_net_codec::FramedProto;
use vertex_swarm_net_headers::{
    HeaderedInbound, HeaderedOutbound, HeaderedStream, Outbound, ProtocolStreamError,
};
use vertex_swarm_net_proto::pingpong::{Ping, Pong};

use crate::PROTOCOL_NAME;
use crate::codec::{Greeting, GreetingEcho, GreetingError};

/// Maximum size of a pingpong message.
const MAX_MESSAGE_SIZE: usize = 4096;

type Framed = FramedProto<MAX_MESSAGE_SIZE>;

/// Convert a typed-payload rejection (over-length) into a protocol stream error.
///
/// We treat any peer that sends an over-long greeting/echo as a misbehaving
/// remote: the protocol cap is well above what bee ever produces.
fn typed_payload_error(direction: &'static str, err: GreetingError) -> ProtocolStreamError {
    ProtocolStreamError::Upgrade(format!("pingpong {direction}: {err}"))
}

/// Pingpong inbound: receives a ping from remote, sends pong response.
#[derive(Debug, Clone)]
pub(crate) struct PingpongInboundInner;

impl HeaderedInbound for PingpongInboundInner {
    type Output = ();
    type Error = ProtocolStreamError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            debug!("Pingpong: Reading ping");
            let (ping, stream) =
                Framed::recv::<Ping, ProtocolStreamError, _>(stream.into_inner()).await?;

            // Enforce typed bounds on the remote payload. A peer that sends an
            // over-long greeting is treated as a protocol error: we never
            // surface unbounded strings to upper layers.
            let greeting =
                Greeting::try_from(ping.greeting).map_err(|e| typed_payload_error("ping", e))?;
            debug!(greeting = %greeting, "Pingpong: Received ping");

            let echo = greeting.echo();
            debug!(response = %echo, "Pingpong: Sending pong");
            Framed::send::<_, ProtocolStreamError, _>(
                stream,
                Pong {
                    response: echo.into_string(),
                },
            )
            .await?;

            Ok(())
        })
    }
}

/// Pingpong outbound: sends a ping, receives pong response. Returns the typed echo.
#[derive(Debug, Clone)]
pub struct PingpongOutboundInner {
    greeting: Greeting,
}

impl PingpongOutboundInner {
    pub fn new(greeting: Greeting) -> Self {
        Self { greeting }
    }
}

impl HeaderedOutbound for PingpongOutboundInner {
    type Output = GreetingEcho;
    type Error = ProtocolStreamError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let start = std::time::Instant::now();

            debug!(greeting = %self.greeting, "Pingpong: Sending ping");
            let stream = Framed::send::<_, ProtocolStreamError, _>(
                stream.into_inner(),
                Ping {
                    greeting: self.greeting.into_string(),
                },
            )
            .await?;

            debug!("Pingpong: Reading pong response");
            let (pong, _) = Framed::recv::<Pong, ProtocolStreamError, _>(stream).await?;

            // Pingpong-specific RTT histogram
            histogram!("pingpong_rtt_seconds").record(start.elapsed().as_secs_f64());

            // Enforce typed bounds on the remote echo payload.
            let echo =
                GreetingEcho::try_from(pong.response).map_err(|e| typed_payload_error("pong", e))?;
            Ok(echo)
        })
    }
}

/// Outbound protocol type for handler.
pub(crate) type PingpongOutboundProtocol = Outbound<PingpongOutboundInner>;

/// Create an outbound protocol handler with the given greeting.
pub(crate) fn outbound(greeting: Greeting) -> PingpongOutboundProtocol {
    Outbound::new(PingpongOutboundInner::new(greeting))
}
