//! Handshake protocol for Swarm peer authentication and identity exchange.

use std::time::Duration;

use libp2p::{Multiaddr, PeerId};
use vertex_swarm_net_core::{ProtoCodec, SemanticVersion, SwarmProtocol, swarm_protocol_id};
use vertex_swarm_peer::{SwarmNodeType, SwarmPeer};

mod behaviour;
pub use behaviour::{HandshakeBehaviour, HandshakeEvent};

mod handler;

mod codec;

mod protocol;

mod error;
pub use error::HandshakeError;

pub mod metrics;
pub use metrics::HandshakeStage;

mod address;
pub use address::{AddressProvider, NoAddresses};

/// Protocol name for handshake.
pub const PROTOCOL: &str = swarm_protocol_id!("handshake", 14, 0, 0, "handshake");

/// Marker codec type for the [`SwarmProtocol`] impl on [`Handshake`].
///
/// Handshake's wire framing lives in `protocol.rs` (built on
/// `vertex_net_codec::FramedProto`); this struct exists only to satisfy the
/// trait's `Codec` slot.
#[derive(Debug)]
pub struct HandshakeCodec;

impl ProtoCodec for HandshakeCodec {
    type Message = HandshakeInfo;
}

/// Marker type identifying the Swarm handshake protocol family.
#[derive(Debug)]
pub struct Handshake;

impl SwarmProtocol for Handshake {
    const NAME: &'static str = "handshake";
    const VERSION: SemanticVersion = SemanticVersion::new(14, 0, 0);
    const STREAM_NAME: &'static str = "handshake";
    type Codec = HandshakeCodec;
    type Message = HandshakeInfo;

    fn full_protocol_id() -> libp2p::StreamProtocol {
        libp2p::StreamProtocol::new(PROTOCOL)
    }
}

/// Timeout for handshake operations.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum length of welcome message in characters.
const MAX_WELCOME_MESSAGE_CHARS: usize = 140;

/// Information from a completed handshake.
#[derive(Clone, Debug)]
pub struct HandshakeInfo {
    pub peer_id: PeerId,
    pub swarm_peer: SwarmPeer,
    /// The peer's node type (capability level).
    pub node_type: SwarmNodeType,
    pub welcome_message: String,
    /// Can be reported to an AddressManager for NAT discovery.
    pub observed_multiaddr: Multiaddr,
}
