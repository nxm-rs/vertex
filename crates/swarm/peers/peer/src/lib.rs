//! Peer identity and addressing primitives for the Ethereum Swarm P2P network.
//!
//! The peer record type is [`SwarmPeer`] (bee handshake 15.0.0). It binds
//! the peer's multiaddrs, overlay, nonce, wall-clock timestamp and
//! an optional chequebook address into a single EIP-191 handshake signature
//! using the canonical sign-data layout from
//! [`nectar_primitives::signing::sign_data`].
//!
//! `Nonce` and `Timestamp` are re-exported from nectar (canonical Swarm
//! primitives); `SwarmAddress` and `SwarmNodeType` are likewise re-exported
//! for ergonomics.

pub mod error;
mod serde_multiaddr;
pub mod swarm_peer;

pub use error::SwarmPeerError;
pub use serde_multiaddr::{deserialize_multiaddrs, serialize_multiaddrs};
pub use swarm_peer::{Nonce, SwarmPeer, SwarmPeerWire, Timestamp};

pub use nectar_primitives::SwarmAddress;
pub use nectar_swarms::{NamedSwarm, Swarm, SwarmKind};
pub use vertex_net_local::AddressScope;
pub use vertex_swarm_primitives::SwarmNodeType;

/// Generate a random valid multiaddr for property testing.
#[cfg(any(test, feature = "test-utils"))]
pub fn arbitrary_multiaddr(
    u: &mut arbitrary::Unstructured<'_>,
) -> arbitrary::Result<libp2p::Multiaddr> {
    use std::net::{Ipv4Addr, Ipv6Addr};

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
