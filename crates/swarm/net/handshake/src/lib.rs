//! Handshake protocol for Swarm peer authentication and identity exchange.
//!
//! Wire protocol id [`PROTOCOL`] = `/swarm/handshake/15.0.0/handshake`. A new
//! connection exchanges syn/synack/ack frames to prove control of an overlay
//! key, agree on the network id, and learn the remote's node type, multiaddrs,
//! and welcome message before the connection is admitted to the topology.
//!
//! # Protocol assumptions not in the Book of Swarm
//!
//! - [`HANDSHAKE_TIMEOUT`] = 15 seconds bounds the whole exchange. The handler
//!   arms it per operation, and the topology reuses it as the dialer's in-flight
//!   timeout and the stale-pending cleanup window. A peer that upgrades the
//!   transport but does not finish the handshake within this window is
//!   disconnected and its slot freed, so a stalled or half-open peer cannot pin
//!   a connection indefinitely.
//! - `MAX_WELCOME_MESSAGE_CHARS` = 140 caps the free-form welcome string. The
//!   limit is enforced on decode in the codec (`welcome_message_from_proto`),
//!   counted in Unicode scalar values rather than bytes, and an over-long
//!   message fails the handshake with a validation error rather than being
//!   truncated. Bounding it stops an untrusted peer from spending our memory on
//!   a field that carries no protocol meaning.
//! - The exchange runs exactly once per connection. A dialer connection
//!   accepts no inbound exchange and a listener connection accepts only the
//!   first; any further attempt is denied before a frame is read (no
//!   signature-recovery work is spent on it) and fails the upgrade with
//!   [`HandshakeError::UnexpectedExchange`], a protocol violation the topology
//!   answers by dropping the connection.
//! - The advertised multiaddr set is bounded before the self record is signed
//!   (see `bound_advertised`): at most `MAX_MULTIADDRS_PER_PEER` entries, and a
//!   serialized block small enough that the whole frame fits a conformant
//!   peer's decode buffer. The frame limit is enforced on decode by both sides,
//!   so an unbounded local set (accumulated listen or verified-external
//!   addresses) would produce a record every peer silently rejects. Bounding is
//!   deterministic prefix truncation of the trust-ordered set, never an encode
//!   error.

use std::time::Duration;

use libp2p::{Multiaddr, PeerId};
use vertex_swarm_peer::{SwarmNodeType, SwarmPeer};

mod behaviour;
pub use behaviour::{HandshakeBehaviour, HandshakeEvent};

mod handler;

mod cache;

mod codec;

mod protocol;

mod error;
pub use error::HandshakeError;

pub mod metrics;
pub use metrics::HandshakeStage;

mod address;
pub use address::{AddressProvider, NoAddresses};

mod admission;
pub use admission::{
    AdmissionDecision, AdmissionRejection, AlwaysAccept, ConnectionDirection,
    HandshakeAdmissionControl, SharedAdmissionControl, default_admission_control,
};

/// Protocol name for handshake.
pub const PROTOCOL: &str = "/swarm/handshake/15.0.0/handshake";

/// Timeout for the full handshake exchange.
///
/// The topology reuses this as the dialer in-flight timeout and the
/// stale-pending cleanup window; see the crate-level docs.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum welcome-message length, in Unicode scalar values.
///
/// Enforced on decode in the codec; an over-long message fails the handshake.
const MAX_WELCOME_MESSAGE_CHARS: usize = 140;

/// Maximum size for handshake message buffers, enforced on decode by the
/// framed codec. The advertised-address bound is derived from it so an
/// outbound frame always fits a conformant peer's buffer.
pub(crate) const MAX_HANDSHAKE_BUFFER_SIZE: usize = 1024;

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
