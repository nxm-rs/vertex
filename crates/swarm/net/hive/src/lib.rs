//! Hive protocol for Swarm peer gossip and network bootstrapping.

mod behaviour;
mod codec;
mod error;
mod handler;
pub mod metrics;
mod protocol;

pub use behaviour::{HiveBehaviour, HiveEvent};
pub use error::ValidationFailure;

use vertex_swarm_net_core::{ProtoCodec, SemanticVersion, SwarmProtocol, swarm_protocol_id};

/// Protocol name for hive.
pub const PROTOCOL_NAME: &str = swarm_protocol_id!("hive", 1, 1, 0, "peers");

/// Maximum number of peers per broadcast message.
pub const MAX_BATCH_SIZE: usize = 30;

/// Marker codec type for the [`SwarmProtocol`] impl on [`Hive`].
///
/// The actual wire framing lives in `protocol.rs`; this struct exists only to
/// satisfy the trait's `Codec` slot and pin the wire message type.
#[derive(Debug)]
pub struct HiveCodec;

impl ProtoCodec for HiveCodec {
    type Message = vertex_swarm_net_proto::hive::Peers;
}

/// Marker type identifying the Swarm hive (peer-exchange) protocol family.
#[derive(Debug)]
pub struct Hive;

impl SwarmProtocol for Hive {
    const NAME: &'static str = "hive";
    const VERSION: SemanticVersion = SemanticVersion::new(1, 1, 0);
    const STREAM_NAME: &'static str = "peers";
    type Codec = HiveCodec;
    type Message = vertex_swarm_net_proto::hive::Peers;

    fn full_protocol_id() -> libp2p::StreamProtocol {
        libp2p::StreamProtocol::new(PROTOCOL_NAME)
    }
}
