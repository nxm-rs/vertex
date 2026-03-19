//! Pingpong protocol for Swarm connection liveness and RTT measurement.

mod behaviour;
mod codec;
mod handler;
pub mod metrics;
mod protocol;

pub use behaviour::{PingpongBehaviour, PingpongEvent};
pub use handler::{PingpongCommand, PingpongConfig, PingpongHandler, PingpongHandlerEvent};
pub use protocol::{PingpongOutboundProtocol, outbound};

/// Protocol name for pingpong.
pub const PROTOCOL_NAME: &str = "/swarm/pingpong/1.0.0/pingpong";
