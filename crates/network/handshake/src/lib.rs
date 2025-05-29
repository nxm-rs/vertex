use std::time::Duration;

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

const PROTOCOL: &str = "/swarm/handshake/13.0.0/handshake";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_WELCOME_MESSAGE_CHARS: usize = 140;

#[derive(Debug, Clone)]
pub struct HandshakeInfo<const N: u64> {
    pub peer_id: PeerId,
    pub ack: Ack<N>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum HandshakeState {
    Idle,
    Handshaking,
    Completed,
    Failed,
}

#[derive(Debug)]
pub enum HandshakeCommand {
    StartHandshake,
}
