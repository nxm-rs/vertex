//! Pingpong protocol for Swarm connection liveness and RTT measurement.

mod codec;
mod error;
mod protocol;

// Include generated protobuf code
#[allow(unreachable_pub)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

pub use codec::{Ping, Pong};
pub use error::PingpongError;
pub use protocol::{PingpongInboundProtocol, PingpongOutboundProtocol, inbound, outbound};

/// Protocol name for pingpong.
pub const PROTOCOL_NAME: &str = "/swarm/pingpong/1.0.0/pingpong";
