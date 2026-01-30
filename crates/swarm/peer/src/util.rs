//! Utility functions for peer identity.

use bytes::{Bytes, BytesMut};
use libp2p::Multiaddr;
use nectar_primitives::SwarmAddress;
use std::net::{Ipv4Addr, Ipv6Addr};

/// Generate the message to sign for handshake verification.
///
/// Format: `"bee-handshake-" || multiaddr_bytes || overlay || network_id(BE)`
pub(crate) fn generate_sign_message(
    multiaddr_bytes: &[u8],
    overlay: &SwarmAddress,
    network_id: u64,
) -> Bytes {
    let mut message = BytesMut::new();
    message.extend_from_slice(b"bee-handshake-");
    message.extend_from_slice(multiaddr_bytes);
    message.extend_from_slice(overlay.as_ref());
    message.extend_from_slice(network_id.to_be_bytes().as_slice());
    message.freeze()
}

/// Generate a random valid multiaddr for property testing.
pub fn arbitrary_multiaddr(u: &mut arbitrary::Unstructured) -> arbitrary::Result<Multiaddr> {
    let use_ipv6: bool = u.arbitrary()?;
    let port: u16 = u.int_in_range(1025..=65535)?;

    let addr = if use_ipv6 {
        let bytes: [u8; 16] = u.arbitrary()?;
        let ipv6 = Ipv6Addr::from(bytes);
        format!("/ip6/{}/tcp/{}", ipv6, port)
    } else {
        let bytes: [u8; 4] = u.arbitrary()?;
        let ipv4 = Ipv4Addr::from(bytes);
        format!("/ip4/{}/tcp/{}", ipv4, port)
    };

    addr.parse().map_err(|_| arbitrary::Error::IncorrectFormat)
}
