//! Protocol upgrade for pingpong.

use futures::future::BoxFuture;
use metrics::histogram;
use tracing::debug;
use vertex_net_codec::FramedProto;
use vertex_swarm_net_headers::{
    HeaderedInbound, HeaderedOutbound, HeaderedStream, Outbound, ProtocolStreamError,
};
use vertex_swarm_net_proto::pingpong::{Ping, Pong};

use crate::codec::format_pong_response;
use crate::PROTOCOL_NAME;

/// Maximum size of a pingpong message.
const MAX_MESSAGE_SIZE: usize = 4096;

type Framed = FramedProto<MAX_MESSAGE_SIZE>;

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

            debug!(greeting = %ping.greeting, "Pingpong: Received ping");
            let response = format_pong_response(&ping.greeting);

            debug!(response = %response, "Pingpong: Sending pong");
            Framed::send::<_, ProtocolStreamError, _>(stream, Pong { response }).await?;

            Ok(())
        })
    }
}

/// Pingpong outbound: sends a ping, receives pong response. Returns the response string.
#[derive(Debug, Clone)]
pub struct PingpongOutboundInner {
    greeting: String,
}

impl PingpongOutboundInner {
    pub fn new(greeting: impl Into<String>) -> Self {
        Self {
            greeting: greeting.into(),
        }
    }
}

impl HeaderedOutbound for PingpongOutboundInner {
    type Output = String;
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
                Ping { greeting: self.greeting },
            )
            .await?;

            debug!("Pingpong: Reading pong response");
            let (pong, _) = Framed::recv::<Pong, ProtocolStreamError, _>(stream).await?;

            // Pingpong-specific RTT histogram
            histogram!("pingpong_rtt_seconds").record(start.elapsed().as_secs_f64());

            Ok(pong.response)
        })
    }
}

/// Outbound protocol type for handler.
pub type PingpongOutboundProtocol = Outbound<PingpongOutboundInner>;

/// Create an outbound protocol handler with the given greeting.
pub fn outbound(greeting: impl Into<String>) -> PingpongOutboundProtocol {
    Outbound::new(PingpongOutboundInner::new(greeting))
}
