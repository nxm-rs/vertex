//! Hive protocol for Swarm peer gossip and network bootstrapping.

mod codec;
pub mod metrics;
mod protocol;

pub use codec::HiveCodecError;
pub use protocol::{HiveInboundProtocol, HiveOutboundProtocol, ValidatedPeers, inbound, outbound};

#[allow(unreachable_pub)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

/// Protocol name for hive.
pub const PROTOCOL_NAME: &str = "/swarm/hive/1.1.0/peers";

/// Maximum number of peers per broadcast message.
pub const MAX_BATCH_SIZE: usize = 30;
