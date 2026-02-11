//! Handshake protocol for Swarm peer authentication and identity exchange.

use std::time::Duration;

use libp2p::{Multiaddr, PeerId};

pub use vertex_swarm_identity::Identity;
pub use vertex_swarm_peer::{SwarmNodeType, SwarmPeer};

#[allow(unreachable_pub)]
pub(crate) mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

mod error;
pub use error::HandshakeError;

pub mod metrics;

mod protocol;
pub use protocol::HandshakeProtocol;

pub mod codec;
pub use codec::{Ack, AckCodec, Syn, SynAck, SynAckCodec, SynCodec};

/// Protocol name for handshake.
pub const PROTOCOL: &str = "/swarm/handshake/14.0.0/handshake";

/// Timeout for handshake operations.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum length of welcome message in characters.
const MAX_WELCOME_MESSAGE_CHARS: usize = 140;

/// Information from a completed handshake.
#[derive(Clone)]
pub struct HandshakeInfo {
    pub peer_id: PeerId,
    pub swarm_peer: SwarmPeer,
    /// The peer's node type (capability level).
    pub node_type: SwarmNodeType,
    pub welcome_message: String,
    /// Can be reported to an AddressManager for NAT discovery.
    pub observed_multiaddr: Multiaddr,
}
