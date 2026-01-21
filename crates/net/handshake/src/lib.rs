use std::{sync::Arc, time::Duration};

use alloy_primitives::B256;
use alloy_signer::k256::ecdsa::SigningKey;
use alloy_signer_local::LocalSigner;
use libp2p::{PeerId, swarm::ConnectionId};

mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}
mod error;
pub use error::HandshakeError;
mod behaviour;
pub use behaviour::HandshakeBehaviour;
mod protocol;
pub use protocol::HandshakeProtocol;
mod handler;
pub use handler::HandshakeHandler;
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

const PROTOCOL: &str = "/swarm/handshake/14.0.0/handshake";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_WELCOME_MESSAGE_CHARS: usize = 140;

#[derive(Debug, Clone)]
pub struct HandshakeInfo {
    pub peer_id: PeerId,
    pub ack: Ack,
}

#[derive(Debug, Clone)]
pub struct PeerState {
    pub info: HandshakeInfo,
    pub connections: Vec<ConnectionId>,
}

#[derive(Debug)]
pub enum HandshakeEvent {
    Completed(HandshakeInfo),
    Failed(HandshakeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HandshakeState {
    Idle,
    Handshaking,
    Completed,
    Failed,
}

#[derive(Debug)]
pub enum HandshakeCommand {
    /// Start the handshake with the resolved remote address.
    ///
    /// The address should be the actual IP address we connected to,
    /// not the DNS address we dialed (e.g., `/ip4/x.x.x.x/tcp/1634`
    /// instead of `/dnsaddr/mainnet.ethswarm.org`).
    StartHandshake(libp2p::Multiaddr),
}
