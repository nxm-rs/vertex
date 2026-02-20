//! Hive protocol for Swarm peer gossip and network bootstrapping.

mod behaviour;
mod codec;
mod handler;
pub mod metrics;
mod protocol;

pub use behaviour::{HiveBehaviour, HiveEvent};
pub use handler::{HiveConfig, HiveHandler, HiveHandlerIn, HiveHandlerOut};
pub use protocol::{HiveInboundProtocol, HiveOutboundProtocol, ValidatedPeers, inbound, outbound};

/// Protocol name for hive.
pub const PROTOCOL_NAME: &str = "/swarm/hive/1.1.0/peers";

/// Maximum number of peers per broadcast message.
pub const MAX_BATCH_SIZE: usize = 30;
