//! Protocol upgrade for pingpong.

use futures::future::BoxFuture;
use tracing::debug;
use vertex_net_codec::FramedProto;
use vertex_swarm_net_headers::{
    HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound, ProtocolStreamError,
};
use vertex_swarm_net_proto::pingpong::{Ping, Pong};

use crate::codec::format_pong_response;
use crate::metrics::PingpongMetrics;
use crate::PROTOCOL_NAME;

/// Maximum size of a pingpong message.
const MAX_MESSAGE_SIZE: usize = 4096;

type Framed = FramedProto<MAX_MESSAGE_SIZE>;

/// Pingpong inbound: receives a ping from remote, sends pong response.
#[derive(Debug, Clone)]
pub struct PingpongInboundInner;

impl HeaderedInbound for PingpongInboundInner {
    type Output = ();
    type Error = ProtocolStreamError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let metrics = PingpongMetrics::inbound();

            debug!("Pingpong: Reading ping");
            let (ping, stream) = match Framed::recv::<Ping, ProtocolStreamError, _>(
                stream.into_inner(),
            )
            .await
            {
                Ok(result) => result,
                Err(e) => {
                    metrics.record_error(&e);
                    return Err(e);
                }
            };

            debug!(greeting = %ping.greeting, "Pingpong: Received ping");
            let response = format_pong_response(&ping.greeting);

            debug!(response = %response, "Pingpong: Sending pong");
            if let Err(e) =
                Framed::send::<_, ProtocolStreamError, _>(stream, Pong { response }).await
            {
                metrics.record_error(&e);
                return Err(e);
            }

            metrics.record_success();
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
            let metrics = PingpongMetrics::outbound();
            let start = std::time::Instant::now();

            debug!(greeting = %self.greeting, "Pingpong: Sending ping");
            let stream = match Framed::send::<_, ProtocolStreamError, _>(
                stream.into_inner(),
                Ping { greeting: self.greeting },
            )
            .await
            {
                Ok(stream) => stream,
                Err(e) => {
                    metrics.record_error(&e);
                    return Err(e);
                }
            };

            debug!("Pingpong: Reading pong response");
            let (pong, _) = match Framed::recv::<Pong, ProtocolStreamError, _>(stream).await {
                Ok(result) => result,
                Err(e) => {
                    metrics.record_error(&e);
                    return Err(e);
                }
            };

            metrics.record_success_with_rtt(start.elapsed().as_secs_f64());
            Ok(pong.response)
        })
    }
}

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
