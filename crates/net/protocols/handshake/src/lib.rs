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

use std::{sync::Arc, time::Duration};

use alloy_primitives::B256;
use alloy_signer::k256::ecdsa::SigningKey;
use alloy_signer_local::LocalSigner;
use libp2p::PeerId;

mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

mod error;
pub use error::HandshakeError;

mod protocol;
pub use protocol::HandshakeProtocol;

mod codec;
pub use codec::*;

/// Configuration trait for the handshake protocol.
///
/// This trait abstracts the node configuration needed for the handshake,
/// allowing the handshake crate to remain independent of specific node implementations.
pub trait HandshakeConfig: Send + Sync + 'static {
    /// Returns the network ID (1 for mainnet, 10 for testnet, etc.)
    fn network_id(&self) -> u64;

    /// Returns the nonce used for address generation.
    fn nonce(&self) -> B256;

    /// Returns the signer for signing handshake messages.
    fn signer(&self) -> Arc<LocalSigner<SigningKey>>;

    /// Returns whether this is a full node.
    fn is_full_node(&self) -> bool;

    /// Returns the welcome message to send to peers.
    fn welcome_message(&self) -> Option<String>;
}

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
