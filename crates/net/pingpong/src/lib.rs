//! Pingpong protocol for Swarm connection liveness.
//!
//! This crate implements the Swarm-specific pingpong protocol for measuring
//! round-trip time between peers. It is compatible with Bee's
//! `/swarm/pingpong/1.0.0/pingpong` protocol.
//!
//! # Protocol
//!
//! - Path: `/swarm/pingpong/1.0.0/pingpong`
//! - Uses headers (like other Swarm protocols)
//! - Request: `Ping { greeting: String }`
//! - Response: `Pong { response: String }` where response is `"{greeting}"`
//!
//! # Flow
//!
//! 1. Client initiates stream and sends `Ping` with a greeting
//! 2. Server receives `Ping` and responds with `Pong` containing `"{greeting}"`
//! 3. Client receives `Pong` and measures RTT

mod behaviour;
mod codec;
mod handler;
mod protocol;

// Include generated protobuf code
mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

pub use behaviour::{PingpongBehaviour, PingpongConfig, PingpongEvent};
pub use codec::{Ping, PingCodec, PingpongCodecError, Pong, PongCodec};
pub use handler::{Command as HandlerCommand, Config as HandlerConfig, Event as HandlerEvent, Handler};
pub use protocol::{inbound, outbound, PingpongInboundProtocol, PingpongOutboundProtocol};

/// Protocol name for pingpong.
pub const PROTOCOL_NAME: &str = "/swarm/pingpong/1.0.0/pingpong";
