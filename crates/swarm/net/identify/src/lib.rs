//! Modified libp2p-identify with targeted push support.
//!
//! Extends the standard identify protocol to support pushing specific addresses
//! to specific peers, enabling faster handshakes with NAT'd peers.

mod behaviour;
mod config;
mod generated;
mod handler;
mod protocol;

pub use behaviour::{Behaviour, Event};
pub use config::Config;
pub use protocol::{Info, PushInfo, UpgradeError};

use libp2p::swarm::StreamProtocol;

/// Protocol name for identify.
pub const PROTOCOL_NAME: StreamProtocol = StreamProtocol::new("/ipfs/id/1.0.0");

/// Protocol name for identify push.
pub const PUSH_PROTOCOL_NAME: StreamProtocol = StreamProtocol::new("/ipfs/id/push/1.0.0");

/// Protocol version advertised to peers.
pub const PROTOCOL_VERSION: &str = "/vertex/1.0.0";

/// Default agent version.
pub const AGENT_VERSION: &str = concat!("vertex/", env!("CARGO_PKG_VERSION"));
