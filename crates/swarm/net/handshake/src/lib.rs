//! Handshake protocol for Swarm peer authentication and identity exchange.
//!
//! Wire-compatible with bee `/swarm/handshake/15.0.0/handshake` — every
//! payload binds the peer's nonce, a wall-clock timestamp and an optional
//! chequebook address into the EIP-191 signature (see
//! [`vertex_swarm_peer::BzzAddress`]).
//!
//! The exchange itself is modelled as a typestate (see [`session`]): each
//! protocol step transitions a zero-sized phase marker so the state machine
//! cannot observe an invalid sequence of operations at compile time.

use std::time::Duration;

use libp2p::{Multiaddr, PeerId};
use vertex_swarm_peer::{BzzAddress, SwarmNodeType, SwarmPeer};

mod behaviour;
pub use behaviour::{HandshakeBehaviour, HandshakeEvent};

mod handler;

mod codec;
pub use codec::{DecodedAck, DecodedSynAck};

mod protocol;

mod error;
pub use error::HandshakeError;

pub mod metrics;
pub use metrics::HandshakeStage;

mod address;
pub use address::{AddressProvider, NoAddresses};

mod welcome;
pub use welcome::{MAX_WELCOME_MESSAGE_CHARS, WelcomeMessage, WelcomeMessageError};

pub mod session;

/// Protocol name for the handshake (matches bee `15.0.0`).
pub const PROTOCOL: &str = "/swarm/handshake/15.0.0/handshake";

/// Timeout for handshake operations.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Information from a completed handshake.
///
/// `bzz_address` carries the verified wire address (including timestamp and
/// chequebook); `swarm_peer` is a legacy projection retained so existing
/// consumers can read overlay/multiaddrs without depending on the bzz module.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HandshakeInfo {
    /// libp2p peer ID this handshake belongs to.
    pub peer_id: PeerId,
    /// Verified bee-mainnet wire address.
    pub bzz_address: BzzAddress,
    /// Legacy projection of `bzz_address` into the [`SwarmPeer`] type.
    pub swarm_peer: SwarmPeer,
    /// The peer's node type (capability level).
    pub node_type: SwarmNodeType,
    /// Welcome message advertised by the peer.
    pub welcome_message: WelcomeMessage,
    /// Can be reported to an `AddressManager` for NAT discovery.
    pub observed_multiaddr: Multiaddr,
}
