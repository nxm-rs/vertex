use std::{sync::Arc, time::Duration};

use alloy::{primitives::B256, signers::local::PrivateKeySigner};
use libp2p::{swarm::ConnectionId, PeerId};

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
use vertex_network_primitives::NodeAddress;

const PROTOCOL: &str = "/swarm/handshake/13.0.0/handshake";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_WELCOME_MESSAGE_LENGTH: usize = 140;

#[derive(Debug, Clone)]
pub struct HandshakeConfig<const N: u64> {
    pub full_node: bool,
    pub welcome_message: String,
    pub validate_overlay: bool,
    pub wallet: Arc<PrivateKeySigner>,
    pub nonce: B256,
}

impl<const N: u64> Default for HandshakeConfig<N> {
    fn default() -> Self {
        Self {
            full_node: true,
            nonce: B256::ZERO,
            welcome_message: "Vertex into the Swarm".to_string(),
            validate_overlay: true,
            wallet: Arc::new(PrivateKeySigner::random()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HandshakeInfo<const N: u64> {
    pub peer_id: PeerId,
    pub address: NodeAddress<N>,
    pub full_node: bool,
    pub welcome_message: String,
}

#[derive(Debug, Clone)]
pub struct PeerState<const N: u64> {
    pub info: HandshakeInfo<N>,
    pub connections: Vec<ConnectionId>,
}

#[derive(Debug)]
pub enum HandshakeEvent<const N: u64> {
    Completed(HandshakeInfo<N>),
    Failed(HandshakeError),
}

#[derive(Debug)]
enum HandshakeState {
    Idle,
    Start,
    Handshaking,
    Completed,
    Failed,
}

#[derive(Debug)]
pub enum HandshakeCommand {
    StartHandshake,
}
