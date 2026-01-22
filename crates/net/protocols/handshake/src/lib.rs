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
//! 1. **Dialer sends SYN**: Contains the observed underlay address
//! 2. **Listener sends SYNACK**: Contains both SYN (echoed) and ACK
//! 3. **Dialer sends ACK**: Confirms identity
//!
//! # Messages
//!
//! - `Syn`: Observed underlay address
//! - `SynAck`: Response with both Syn and Ack
//! - `Ack`: Node address, full node status, welcome message

use std::time::Duration;

use libp2p::PeerId;

// Re-export SwarmIdentity for convenience
pub use vertex_node_identity::SwarmIdentity;

mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

mod error;
pub use error::HandshakeError;

mod protocol;
pub use protocol::HandshakeProtocol;

mod codec;
pub use codec::*;

/// Protocol name for handshake.
pub const PROTOCOL: &str = "/swarm/handshake/14.0.0/handshake";

/// Timeout for handshake operations.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum length of welcome message in characters.
const MAX_WELCOME_MESSAGE_CHARS: usize = 140;

/// Information from a completed handshake.
#[derive(Debug, Clone)]
pub struct HandshakeInfo {
    /// The peer ID of the remote peer.
    pub peer_id: PeerId,
    /// The ACK message containing peer identity.
    pub ack: Ack,
}
