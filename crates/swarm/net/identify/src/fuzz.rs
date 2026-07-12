//! Fuzz-facing surface behind the `arbitrary` feature: byte-level decode
//! through the identify message conversions over the vendored generated
//! struct, plus generators. Dev-only; never enabled by a shipped artefact
//! cone.

use arbitrary::Unstructured;
use libp2p::core::PeerRecord;
use libp2p::identity::Keypair;
use libp2p::multiaddr::{Multiaddr, Protocol};
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};

use crate::generated::Identify;

pub use crate::error::UpgradeError;
pub use crate::protocol::{Info, PushInfo};

/// Frame cap the framed identify codec enforces on decode.
pub const MAX_MESSAGE_SIZE_BYTES: usize = crate::protocol::MAX_MESSAGE_SIZE_BYTES;

/// Both inbound conversions of one decoded message.
pub type DecodedIdentify = (Result<Info, UpgradeError>, Result<PushInfo, UpgradeError>);

/// Generated-reader decode, then both domain conversions; the exact sites the
/// inbound identify and push paths run. `None` when the reader rejects.
pub fn decode_identify(data: &[u8]) -> Option<DecodedIdentify> {
    let mut reader = BytesReader::from_bytes(data);
    let msg = Identify::from_reader(&mut reader, data).ok()?;
    Some((Info::try_from(msg.clone()), PushInfo::try_from(msg)))
}

/// A random socket multiaddr.
fn arbitrary_multiaddr(u: &mut Unstructured<'_>) -> arbitrary::Result<Multiaddr> {
    let ip: [u8; 4] = u.arbitrary()?;
    let port: u16 = u.arbitrary()?;
    Ok(Multiaddr::from(std::net::Ipv4Addr::from(ip)).with(Protocol::Tcp(port)))
}

/// A multiaddr byte field: usually well-formed, otherwise hostile bytes.
fn arbitrary_addr_bytes(u: &mut Unstructured<'_>) -> arbitrary::Result<Vec<u8>> {
    if u.arbitrary()? {
        Ok(arbitrary_multiaddr(u)?.to_vec())
    } else {
        u.arbitrary()
    }
}

/// A deterministic ed25519 keypair drawn from `u`.
fn arbitrary_keypair(u: &mut Unstructured<'_>) -> arbitrary::Result<Keypair> {
    Keypair::ed25519_from_bytes(u.arbitrary::<[u8; 32]>()?)
        .map_err(|_| arbitrary::Error::IncorrectFormat)
}

/// Wire bytes of an arbitrary identify message. Arms cover a really decodable
/// public key and a really signed peer record (signed by that key or by an
/// unrelated one) so the validated conversion paths stay reachable.
pub fn arbitrary_identify_bytes(u: &mut Unstructured<'_>) -> arbitrary::Result<Vec<u8>> {
    let keypair = arbitrary_keypair(u)?;
    let public_key = match u.int_in_range(0..=2u8)? {
        0 => None,
        1 => Some(u.arbitrary()?),
        _ => Some(keypair.public().encode_protobuf()),
    };
    let listen_addrs = (0..u.int_in_range(0..=4u8)?)
        .map(|_| arbitrary_addr_bytes(u))
        .collect::<arbitrary::Result<Vec<_>>>()?;
    let observed_addr = if u.arbitrary()? {
        Some(arbitrary_addr_bytes(u)?)
    } else {
        None
    };
    let signed_peer_record = match u.int_in_range(0..=2u8)? {
        0 => None,
        1 => Some(u.arbitrary()?),
        _ => {
            let signer = if u.arbitrary()? {
                keypair
            } else {
                arbitrary_keypair(u)?
            };
            let addrs = (0..u.int_in_range(0..=3u8)?)
                .map(|_| arbitrary_multiaddr(u))
                .collect::<arbitrary::Result<Vec<_>>>()?;
            PeerRecord::new(&signer, addrs)
                .ok()
                .map(|r| r.into_signed_envelope().into_protobuf_encoding())
        }
    };
    let msg = Identify {
        protocolVersion: u.arbitrary()?,
        agentVersion: u.arbitrary()?,
        publicKey: public_key,
        listenAddrs: listen_addrs,
        observedAddr: observed_addr,
        protocols: u.arbitrary()?,
        signedPeerRecord: signed_peer_record,
    };
    let mut bytes = Vec::with_capacity(msg.get_size());
    msg.write_message(&mut Writer::new(&mut bytes))
        .map_err(|_| arbitrary::Error::IncorrectFormat)?;
    Ok(bytes)
}
