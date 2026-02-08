//! Handshake protocol for Swarm peer authentication.
//!
//! The handshake protocol is the first protocol run when two Swarm peers connect.
//! It exchanges identity information (overlay addresses, network IDs, etc.) and
//! verifies that both peers are on the same network.
//!
//! # Protocol
//!
//! - Path: `/swarm/handshake/14.0.0/handshake`
//! - Three-way handshake: SYN → SYNACK → ACK
//!
//! # Flow
//!
//! 1. **Dialer sends SYN**: Contains the observed multiaddr address
//! 2. **Listener sends SYNACK**: Contains both SYN (echoed) and ACK
//! 3. **Dialer sends ACK**: Confirms identity
//!
//! # Messages
//!
//! - `Syn`: Observed multiaddr address
//! - `SynAck`: Response with both Syn and Ack
//! - `Ack`: Node address, full node status, welcome message

use std::time::Duration;

use libp2p::{Multiaddr, PeerId};
use vertex_swarm_peer::SwarmPeer;

// Re-export Identity for convenience
pub use vertex_swarm_identity::Identity;

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
pub use codec::{
    Ack, AckCodec, CodecError, HandshakeCodecDomainError, Syn, SynAck, SynAckCodec, SynCodec,
};

/// Protocol name for handshake.
pub const PROTOCOL: &str = "/swarm/handshake/14.0.0/handshake";

/// Timeout for handshake operations.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum length of welcome message in characters.
const MAX_WELCOME_MESSAGE_CHARS: usize = 140;

/// Information from a completed handshake.
///
/// Contains the remote peer's cryptographic identity (`SwarmPeer`) and
/// metadata about the peer (full node status, welcome message).
#[derive(Clone)]
pub struct HandshakeInfo {
    pub peer_id: PeerId,
    pub swarm_peer: SwarmPeer,
    pub full_node: bool,
    pub welcome_message: String,
    /// Can be reported to an AddressManager for NAT discovery.
    pub observed_multiaddr: Multiaddr,
}

