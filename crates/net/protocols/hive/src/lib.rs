//! Hive protocol for Swarm peer discovery.
//!
//! Peers gossip known addresses to each other for network bootstrapping.
//!
//! # Protocol
//!
//! - Path: `/swarm/hive/1.1.0/peers`
//! - Unidirectional: sender broadcasts, receiver processes
//! - Message: `Peers` containing peer addresses
//!
//! # Batching
//!
//! Large peer lists are split into batches of at most 30 peers.

mod codec;
mod protocol;

pub use codec::{HiveCodec, HiveCodecError, Peers};
pub use protocol::{HiveInboundProtocol, HiveOutboundProtocol, ValidatedPeers, inbound, outbound};

mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

/// Protocol name for hive.
pub const PROTOCOL_NAME: &str = "/swarm/hive/1.1.0/peers";

/// Maximum number of peers per broadcast message.
pub const MAX_BATCH_SIZE: usize = 30;
