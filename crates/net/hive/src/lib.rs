//! Hive protocol for Swarm peer discovery.
//!
//! The hive protocol allows peers to gossip known peer addresses to each other.
//! This enables peer discovery and network bootstrapping.
//!
//! # Protocol
//!
//! - Path: `/swarm/hive/1.1.0/peers`
//! - Unidirectional: Sender broadcasts, receiver processes
//! - Message: `Peers` containing repeated `BzzAddress` structures
//!
//! # Flow
//!
//! 1. Sender opens a stream to a peer
//! 2. Sender writes a `Peers` message with peer addresses
//! 3. Receiver reads the message and validates peers
//! 4. Stream is closed
//!
//! # Batching
//!
//! Large peer lists are split into batches of at most 30 peers.
//! Each batch is sent as a separate stream.

mod behaviour;
mod codec;
mod handler;
mod protocol;

pub use behaviour::{HiveBehaviour, HiveConfig, HiveEvent, MAX_BATCH_SIZE};
pub use codec::{BzzAddress, HiveCodec, HiveCodecError, Peers};
pub use handler::{Command as HandlerCommand, Config as HandlerConfig, Event as HandlerEvent, Handler};
pub use protocol::{
    HiveError, HiveInboundOutput, HiveInboundProtocol, HiveOutboundOutput, HiveOutboundProtocol,
};

mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

/// Protocol name for hive.
pub const PROTOCOL_NAME: &str = "/swarm/hive/1.1.0/peers";
