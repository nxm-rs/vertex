//! Pingpong protocol for Swarm connection liveness and RTT measurement.

mod behaviour;
pub use behaviour::{PingpongBehaviour, PingpongEvent};

mod handler;

mod codec;

mod protocol;
pub(crate) use protocol::{PingpongOutboundProtocol, outbound};

pub mod metrics;

use vertex_swarm_net_core::{ProtoCodec, SemanticVersion, SwarmProtocol, swarm_protocol_id};

/// Protocol name for pingpong.
pub const PROTOCOL_NAME: &str = swarm_protocol_id!("pingpong", 1, 0, 0, "pingpong");

/// Marker codec type for the [`SwarmProtocol`] impl on [`Pingpong`].
///
/// The actual wire framing lives in `protocol.rs`; this struct exists only to
/// satisfy the trait's `Codec` slot and pin the wire message type.
#[derive(Debug)]
pub struct PingpongCodec;

impl ProtoCodec for PingpongCodec {
    type Message = vertex_swarm_net_proto::pingpong::Ping;
}

/// Marker type identifying the Swarm pingpong protocol family.
#[derive(Debug)]
pub struct Pingpong;

impl SwarmProtocol for Pingpong {
    const NAME: &'static str = "pingpong";
    const VERSION: SemanticVersion = SemanticVersion::new(1, 0, 0);
    const STREAM_NAME: &'static str = "pingpong";
    type Codec = PingpongCodec;
    type Message = vertex_swarm_net_proto::pingpong::Ping;

    fn full_protocol_id() -> libp2p::StreamProtocol {
        libp2p::StreamProtocol::new(PROTOCOL_NAME)
    }
}
