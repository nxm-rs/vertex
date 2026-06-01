//! Pingpong protocol for Swarm connection liveness and RTT measurement.

mod behaviour;
pub use behaviour::{PingpongBehaviour, PingpongEvent};

mod handler;

mod codec;
pub use codec::{Greeting, GreetingEcho, GreetingError, MAX_GREETING_CHARS, PingpongMessage};

mod protocol;
pub(crate) use protocol::{PingpongOutboundProtocol, outbound};

pub mod metrics;

/// Protocol name for pingpong.
pub const PROTOCOL_NAME: &str = "/swarm/pingpong/1.0.0/pingpong";
