//! Fuzz-facing surface behind the `arbitrary` feature: the raw decode entry
//! points, the frame-sizing arithmetic, and valid-by-construction generators
//! for the handshake frames. Dev-only; never enabled by a shipped artefact
//! cone.

use arbitrary::{Arbitrary, Unstructured};

use crate::HandshakeError;
use crate::codec::encode_swarm_peer;

pub use libp2p::Multiaddr;
pub use nectar_primitives::NetworkId;
pub use vertex_swarm_net_proto::handshake as proto;
pub use vertex_swarm_peer::{
    ARBITRARY_NETWORK_ID, MAX_MULTIADDRS_PER_PEER, SwarmNodeType, SwarmPeer, arbitrary_multiaddr,
    deserialize_multiaddrs, serialize_multiaddrs,
};

/// Frame cap the framed handshake codec enforces on decode.
pub const MAX_HANDSHAKE_BUFFER_SIZE: usize = crate::MAX_HANDSHAKE_BUFFER_SIZE;

/// Frame bytes [`bound_advertised`] reserves for the fixed fields.
pub const FRAME_FIXED_BUDGET: usize = crate::address::FRAME_FIXED_BUDGET;

/// Welcome-message decode cap, in Unicode scalar values.
pub const MAX_WELCOME_MESSAGE_CHARS: usize = crate::MAX_WELCOME_MESSAGE_CHARS;

/// Decode a syn frame; the exact path the protocol runs on raw peer bytes.
pub fn decode_syn(
    proto: vertex_swarm_net_proto::handshake::Syn,
) -> Result<Multiaddr, HandshakeError> {
    crate::codec::decode_syn(proto)
}

/// Decode an ack frame, running signature recovery and overlay validation.
pub fn decode_ack(
    proto: vertex_swarm_net_proto::handshake::Ack,
    expected_network_id: NetworkId,
) -> Result<(SwarmPeer, SwarmNodeType, String), HandshakeError> {
    crate::codec::decode_ack(proto, expected_network_id)
}

/// Decode a synack frame; the outbound side's view of the exchange.
pub fn decode_synack(
    proto: vertex_swarm_net_proto::handshake::SynAck,
    expected_network_id: NetworkId,
) -> Result<(Multiaddr, SwarmPeer, SwarmNodeType, String), HandshakeError> {
    crate::codec::decode_synack(proto, expected_network_id)
}

/// Encode a syn frame.
pub fn encode_syn(observed: &Multiaddr) -> vertex_swarm_net_proto::handshake::Syn {
    crate::codec::encode_syn(observed)
}

/// Encode an ack frame.
pub fn encode_ack(
    peer: &SwarmPeer,
    node_type: SwarmNodeType,
    welcome_message: &str,
    network_id: NetworkId,
) -> vertex_swarm_net_proto::handshake::Ack {
    crate::codec::encode_ack(peer, node_type, welcome_message, network_id)
}

/// Encode a synack frame.
pub fn encode_synack(
    observed: &Multiaddr,
    peer: &SwarmPeer,
    node_type: SwarmNodeType,
    welcome_message: &str,
    network_id: NetworkId,
) -> vertex_swarm_net_proto::handshake::SynAck {
    crate::codec::encode_synack(observed, peer, node_type, welcome_message, network_id)
}

/// Bound the advertised multiaddr set to the frame budget.
pub fn bound_advertised(addrs: Vec<Multiaddr>, welcome_bytes: usize) -> Vec<Multiaddr> {
    crate::address::bound_advertised(addrs, welcome_bytes)
}

/// Predicted serialized size of one multiaddr list entry.
pub fn entry_len(addr: &Multiaddr) -> usize {
    crate::address::entry_len(addr)
}

/// Predicted encoded size of one uvarint.
pub fn uvarint_len(value: u64) -> usize {
    crate::address::uvarint_len(value)
}

/// A syn input: the observed multiaddr the dialer reports.
#[derive(Debug)]
pub struct ArbitrarySyn {
    pub observed: Multiaddr,
}

impl<'a> Arbitrary<'a> for ArbitrarySyn {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self {
            observed: arbitrary_multiaddr(u)?,
        })
    }
}

/// An ack input: a really signed peer record under [`ARBITRARY_NETWORK_ID`],
/// a wire-representable node type (the boolean storer flag collapses bootnode
/// onto client), and a welcome message inside the decode cap.
#[derive(Debug)]
pub struct ArbitraryAck {
    pub peer: SwarmPeer,
    pub node_type: SwarmNodeType,
    pub welcome: String,
}

impl<'a> Arbitrary<'a> for ArbitraryAck {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let peer = SwarmPeer::arbitrary(u)?;
        let node_type = if u.arbitrary()? {
            SwarmNodeType::Storer
        } else {
            SwarmNodeType::Client
        };
        let welcome_chars = u.int_in_range(0..=MAX_WELCOME_MESSAGE_CHARS)?;
        let welcome = (0..welcome_chars)
            .map(|_| u.arbitrary::<char>())
            .collect::<arbitrary::Result<String>>()?;
        Ok(Self {
            peer,
            node_type,
            welcome,
        })
    }
}

/// A synack input: one syn and one ack.
#[derive(Debug, Arbitrary)]
pub struct ArbitrarySynack {
    pub syn: ArbitrarySyn,
    pub ack: ArbitraryAck,
}

// --- Adversarial wire builders for the decode target ---
//
// These build proto structs with attacker-shaped field content (wrong-length
// overlay/nonce/signature, malformed multiaddr blocks, over-long welcome,
// mismatched network id) so the domain decode runs its full validation:
// multiaddr block parsing with its count cap, the length checks, EIP-191
// signature recovery, and the network-id and welcome gates. Half the time the
// record is a really signed one so the recovery success path stays covered.

/// Draw a bytes field: usually adversarial (any length, so length checks and
/// the hand-rolled multiaddr/uvarint decode see hostile input), occasionally a
/// well-formed multiaddr block so the happy path is reachable.
fn wire_bytes(u: &mut Unstructured<'_>) -> arbitrary::Result<Vec<u8>> {
    if u.arbitrary()? {
        let count = u.int_in_range(0..=MAX_MULTIADDRS_PER_PEER + 4)?;
        let addrs = (0..count)
            .map(|_| arbitrary_multiaddr(u))
            .collect::<arbitrary::Result<Vec<_>>>()?;
        Ok(serialize_multiaddrs(&addrs))
    } else {
        Ok(u.arbitrary()?)
    }
}

/// An adversarial proto `SwarmPeer`, or a really signed one under
/// [`ARBITRARY_NETWORK_ID`] so signature recovery succeeds on that arm.
fn arbitrary_wire_peer(u: &mut Unstructured<'_>) -> arbitrary::Result<proto::SwarmPeer> {
    if u.arbitrary()? {
        return Ok(encode_swarm_peer(&SwarmPeer::arbitrary(u)?));
    }
    Ok(proto::SwarmPeer {
        multiaddrs: wire_bytes(u)?,
        signature: u.arbitrary()?,
        overlay: u.arbitrary()?,
        nonce: u.arbitrary()?,
        timestamp: u.arbitrary()?,
        chequebook_address: u.arbitrary()?,
    })
}

/// A syn frame with adversarial observed-multiaddr bytes.
pub fn arbitrary_wire_syn(u: &mut Unstructured<'_>) -> arbitrary::Result<proto::Syn> {
    Ok(proto::Syn {
        observed_multiaddr: wire_bytes(u)?,
    })
}

/// An ack frame plus the network id its first decode attempt should use (the
/// id carried in the frame, so the recovery paths are not masked by the id
/// gate). A second decode against a fixed id keeps the mismatch arm covered.
pub fn arbitrary_wire_ack(u: &mut Unstructured<'_>) -> arbitrary::Result<(proto::Ack, NetworkId)> {
    let address = if u.arbitrary()? {
        Some(arbitrary_wire_peer(u)?)
    } else {
        None
    };
    let network_id = u.arbitrary()?;
    let welcome_len = u.int_in_range(0..=MAX_WELCOME_MESSAGE_CHARS + 8)?;
    let welcome_message = (0..welcome_len)
        .map(|_| u.arbitrary::<char>())
        .collect::<arbitrary::Result<String>>()?;
    let ack = proto::Ack {
        address,
        network_id,
        storer: u.arbitrary()?,
        welcome_message,
    };
    Ok((ack, NetworkId::new(network_id)))
}

/// A synack frame plus the network id its first decode attempt should use.
pub fn arbitrary_wire_synack(
    u: &mut Unstructured<'_>,
) -> arbitrary::Result<(proto::SynAck, NetworkId)> {
    let syn = if u.arbitrary()? {
        Some(arbitrary_wire_syn(u)?)
    } else {
        None
    };
    let (ack, network_id) = arbitrary_wire_ack(u)?;
    let synack = proto::SynAck {
        syn,
        ack: if u.arbitrary()? { Some(ack) } else { None },
    };
    Ok((synack, network_id))
}
