//! Peer identity and addressing primitives for the Ethereum Swarm P2P network.
//!
//! - [`SwarmPeer`] - Canonical peer identity type
//! - [`SwarmIdentityExt`] - Extension trait for creating peers from identities
//! - Multiaddr serialization (Bee-compatible)
//! - Signature verification and overlay validation

mod serde_multiaddr;

pub use serde_multiaddr::{deserialize_multiaddrs, serialize_multiaddrs};

use bytes::{Bytes, BytesMut};
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_primitives::compute_overlay;
use vertex_swarm_spec::SwarmSpec;

pub use nectar_primitives::SwarmAddress;
pub use nectar_swarms::{NamedSwarm, Swarm, SwarmKind};
pub use vertex_swarm_primitives::SwarmNodeType;

use alloy_primitives::{Address, B256, Signature};
use alloy_signer::{Signer, SignerSync};
use libp2p::Multiaddr;
use vertex_net_local::{IpCapability, classify_multiaddr};

pub use vertex_net_local::AddressScope;

/// Errors from multiaddr serialization/deserialization.
#[derive(Debug, thiserror::Error)]
pub enum MultiAddrError {
    #[error("empty byte slice")]
    EmptyData,
    #[error("failed to read varint: {0}")]
    VarintError(#[from] std::io::Error),
    #[error("inconsistent data: expected {expected} bytes, got {actual}")]
    InconsistentLength { expected: u64, actual: usize },
    #[error("failed to parse multiaddr: {0}")]
    InvalidMultiaddr(#[from] libp2p::multiaddr::Error),
}

/// Errors from [`SwarmPeer`] construction.
#[derive(Debug, thiserror::Error)]
pub enum SwarmPeerError {
    #[error("invalid signature: {0}")]
    InvalidSignature(#[from] alloy_primitives::SignatureError),
    #[error("signer error: {0}")]
    SignerError(#[from] alloy_signer::Error),
    #[error("computed overlay does not match claimed overlay")]
    InvalidOverlay,
    #[error("at least one multiaddr is required")]
    NoMultiaddrs,
    #[error("invalid multiaddr encoding: {0}")]
    InvalidMultiaddrEncoding(#[from] MultiAddrError),
}

/// Verifiable peer identity with multiaddrs, signature, and overlay address.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SwarmPeer {
    multiaddrs: Vec<Multiaddr>,
    signature: Signature,
    overlay: SwarmAddress,
    nonce: B256,
    ethereum_address: Address,
}

impl Default for SwarmPeer {
    /// Creates a placeholder SwarmPeer with zero values.
    ///
    /// Only use for deserialization defaults. Real peers should be created via
    /// `from_identity`, `from_signed`, or `from_validated`.
    fn default() -> Self {
        use alloy_primitives::U256;
        Self {
            multiaddrs: Vec::new(),
            signature: Signature::new(U256::ZERO, U256::ZERO, false),
            overlay: SwarmAddress::default(),
            nonce: B256::ZERO,
            ethereum_address: Address::ZERO,
        }
    }
}

impl SwarmPeer {
    /// Create a `SwarmPeer` from an identity and observed multiaddrs.
    ///
    /// Signs the multiaddrs with the identity's signer to create a verifiable peer.
    /// At least one multiaddr is required - peers must be dialable.
    pub fn from_identity<I: SwarmIdentity + ?Sized>(
        identity: &I,
        multiaddrs: Vec<Multiaddr>,
    ) -> Result<Self, SwarmPeerError> {
        if multiaddrs.is_empty() {
            return Err(SwarmPeerError::NoMultiaddrs);
        }

        let signer = identity.signer();
        let nonce = identity.nonce();
        let network_id = identity.spec().network_id();

        let ethereum_address = signer.address();
        let overlay = compute_overlay(&ethereum_address, network_id, &nonce);

        let multiaddr_bytes = serialize_multiaddrs(&multiaddrs);
        let msg = generate_sign_message(&multiaddr_bytes, &overlay, network_id);
        let signature = signer.sign_message_sync(&msg)?;

        Ok(Self {
            multiaddrs,
            signature,
            overlay,
            nonce,
            ethereum_address,
        })
    }

    /// Create a `SwarmPeer` from protocol data, recovering the ethereum address from signature.
    ///
    /// At least one multiaddr is required - peers without dialable addresses are rejected.
    /// Use the connection's remote address as fallback when creating peers.
    pub fn from_signed(
        multiaddrs_bytes: &[u8],
        signature: Signature,
        overlay: SwarmAddress,
        nonce: B256,
        network_id: u64,
        validate_overlay: bool,
    ) -> Result<Self, SwarmPeerError> {
        let multiaddrs = deserialize_multiaddrs(multiaddrs_bytes)?;
        if multiaddrs.is_empty() {
            return Err(SwarmPeerError::NoMultiaddrs);
        }

        let ethereum_address = recover_signer(multiaddrs_bytes, &overlay, &signature, network_id)?;

        if validate_overlay {
            let expected_overlay = compute_overlay(&ethereum_address, network_id, &nonce);
            if expected_overlay != overlay {
                return Err(SwarmPeerError::InvalidOverlay);
            }
        }

        Ok(Self {
            multiaddrs,
            signature,
            overlay,
            nonce,
            ethereum_address,
        })
    }

    /// Create a `SwarmPeer` from pre-validated data (no verification performed).
    pub fn from_validated(
        multiaddrs: Vec<Multiaddr>,
        signature: Signature,
        overlay: B256,
        nonce: B256,
        ethereum_address: Address,
    ) -> Self {
        Self {
            multiaddrs,
            signature,
            overlay: SwarmAddress::from(overlay),
            nonce,
            ethereum_address,
        }
    }

    pub fn multiaddrs(&self) -> &[Multiaddr] {
        &self.multiaddrs
    }

    pub fn multiaddr(&self) -> Option<&Multiaddr> {
        self.multiaddrs.first()
    }

    pub fn signature(&self) -> &Signature {
        &self.signature
    }

    pub fn overlay(&self) -> &SwarmAddress {
        &self.overlay
    }

    pub fn nonce(&self) -> &B256 {
        &self.nonce
    }

    pub fn ethereum_address(&self) -> &Address {
        &self.ethereum_address
    }

    pub fn serialize_multiaddrs(&self) -> Vec<u8> {
        serialize_multiaddrs(&self.multiaddrs)
    }

    /// Compute IP capability from multiaddrs.
    pub fn ip_capability(&self) -> IpCapability {
        IpCapability::from_addrs(&self.multiaddrs)
    }

    /// Filter addresses by scope.
    pub fn addrs_by_scope(&self, scope: AddressScope) -> Vec<Multiaddr> {
        self.multiaddrs
            .iter()
            .filter(|addr| classify_multiaddr(addr) == Some(scope))
            .cloned()
            .collect()
    }

    /// Check if peer has any addresses of the given scope.
    pub fn has_scope(&self, scope: AddressScope) -> bool {
        self.multiaddrs
            .iter()
            .any(|addr| classify_multiaddr(addr) == Some(scope))
    }

    /// Get the highest scope (Public > LinkLocal > Private > Loopback).
    pub fn max_scope(&self) -> Option<AddressScope> {
        self.multiaddrs
            .iter()
            .filter_map(classify_multiaddr)
            .max_by_key(scope_rank)
    }
}

fn scope_rank(scope: &AddressScope) -> u8 {
    match scope {
        AddressScope::Public => 3,
        AddressScope::LinkLocal => 2,
        AddressScope::Private => 1,
        AddressScope::Loopback => 0,
    }
}

/// Recover the signer's ethereum address from signature and message components.
fn recover_signer(
    multiaddr_bytes: &[u8],
    overlay: &SwarmAddress,
    signature: &Signature,
    network_id: u64,
) -> Result<Address, SwarmPeerError> {
    let prehash = generate_sign_message(multiaddr_bytes, overlay, network_id);
    Ok(signature.recover_address_from_msg(prehash)?)
}

/// Generate the message to sign for handshake verification.
///
/// Format: `"bee-handshake-" || multiaddr_bytes || overlay || network_id(BE)`
fn generate_sign_message(multiaddr_bytes: &[u8], overlay: &SwarmAddress, network_id: u64) -> Bytes {
    let mut message = BytesMut::new();
    message.extend_from_slice(b"bee-handshake-");
    message.extend_from_slice(multiaddr_bytes);
    message.extend_from_slice(overlay.as_ref());
    message.extend_from_slice(network_id.to_be_bytes().as_slice());
    message.freeze()
}

/// Generate a random valid multiaddr for property testing.
#[cfg(any(test, feature = "test-utils"))]
pub fn arbitrary_multiaddr(u: &mut arbitrary::Unstructured) -> arbitrary::Result<Multiaddr> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vertex_swarm_identity::Identity;
    use vertex_swarm_primitives::SwarmNodeType;
    use vertex_swarm_spec::{SpecBuilder, init_testnet};

    #[test]
    fn swarm_peer_roundtrip() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        let peer1 = SwarmPeer::from_identity(&identity, vec![multiaddr]).unwrap();

        let multiaddr_bytes = peer1.serialize_multiaddrs();
        let peer2 = SwarmPeer::from_signed(
            &multiaddr_bytes,
            *peer1.signature(),
            *peer1.overlay(),
            identity.nonce(),
            spec.network_id(),
            true,
        )
        .unwrap();

        assert_eq!(peer1, peer2);
    }

    #[test]
    fn signature_recovery() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        let peer = SwarmPeer::from_identity(&identity, vec![multiaddr]).unwrap();

        let multiaddr_bytes = peer.serialize_multiaddrs();
        let recovered = recover_signer(
            &multiaddr_bytes,
            peer.overlay(),
            peer.signature(),
            spec.network_id(),
        )
        .unwrap();

        assert_eq!(recovered, identity.ethereum_address());
    }

    #[test]
    fn invalid_overlay_rejected() {
        let spec = init_testnet();
        let identity1 = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let identity2 = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        let peer1 = SwarmPeer::from_identity(&identity1, vec![multiaddr]).unwrap();

        // Try to verify peer1's signature with identity2's overlay
        let result = SwarmPeer::from_signed(
            &peer1.serialize_multiaddrs(),
            *peer1.signature(),
            identity2.overlay_address(),
            identity1.nonce(),
            spec.network_id(),
            true,
        );

        assert!(matches!(result, Err(SwarmPeerError::InvalidOverlay)));
    }

    #[test]
    fn different_networks_different_overlays() {
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        // Create specs with different network IDs
        let spec1 = SpecBuilder::testnet().network_id(1).build();
        let spec2 = SpecBuilder::testnet().network_id(2).build();
        let spec3 = SpecBuilder::testnet().network_id(100).build();

        let identity1 = Identity::random(Arc::new(spec1), SwarmNodeType::Storer);
        let identity2 = Identity::random(Arc::new(spec2), SwarmNodeType::Storer);
        let identity3 = Identity::random(Arc::new(spec3), SwarmNodeType::Storer);

        let peer1 = SwarmPeer::from_identity(&identity1, vec![multiaddr.clone()]).unwrap();
        let peer2 = SwarmPeer::from_identity(&identity2, vec![multiaddr.clone()]).unwrap();
        let peer3 = SwarmPeer::from_identity(&identity3, vec![multiaddr]).unwrap();

        assert_ne!(peer1.overlay(), peer2.overlay());
        assert_ne!(peer1.overlay(), peer3.overlay());
        assert_ne!(peer2.overlay(), peer3.overlay());
    }

    #[test]
    fn empty_multiaddrs_rejected_from_identity() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);

        let result = SwarmPeer::from_identity(&identity, vec![]);
        assert!(matches!(result, Err(SwarmPeerError::NoMultiaddrs)));
    }

    #[test]
    fn empty_multiaddrs_rejected_from_signed() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        // Create a valid peer first
        let peer = SwarmPeer::from_identity(&identity, vec![multiaddr]).unwrap();

        // Try to create from signed with empty multiaddrs bytes
        let empty_bytes = serialize_multiaddrs(&[]);
        let result = SwarmPeer::from_signed(
            &empty_bytes,
            *peer.signature(),
            *peer.overlay(),
            identity.nonce(),
            spec.network_id(),
            false,
        );

        assert!(matches!(result, Err(SwarmPeerError::NoMultiaddrs)));
    }

    #[test]
    fn ip_capability_v4_only() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let v4_addr: Multiaddr = "/ip4/192.168.1.1/tcp/1234".parse().unwrap();

        let peer = SwarmPeer::from_identity(&identity, vec![v4_addr]).unwrap();
        let cap = peer.ip_capability();

        assert!(cap.supports_ipv4());
        assert!(!cap.supports_ipv6());
    }

    #[test]
    fn ip_capability_dual_stack() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let v4_addr: Multiaddr = "/ip4/192.168.1.1/tcp/1234".parse().unwrap();
        let v6_addr: Multiaddr = "/ip6/::1/tcp/1234".parse().unwrap();

        let peer = SwarmPeer::from_identity(&identity, vec![v4_addr, v6_addr]).unwrap();
        let cap = peer.ip_capability();

        assert!(cap.supports_ipv4());
        assert!(cap.supports_ipv6());
        assert_eq!(cap, IpCapability::Dual);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_roundtrip_preserves_capability() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let v4_addr: Multiaddr = "/ip4/192.168.1.1/tcp/1234".parse().unwrap();

        let peer1 = SwarmPeer::from_identity(&identity, vec![v4_addr]).unwrap();

        let bytes = postcard::to_allocvec(&peer1).unwrap();
        let peer2: SwarmPeer = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(peer1.ip_capability(), peer2.ip_capability());
        assert!(peer2.ip_capability().supports_ipv4());
    }

    #[test]
    fn max_scope_public() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let addrs = vec![
            "/ip4/127.0.0.1/tcp/1234".parse().unwrap(),
            "/ip4/192.168.1.1/tcp/1234".parse().unwrap(),
            "/ip4/8.8.8.8/tcp/1234".parse().unwrap(),
        ];

        let peer = SwarmPeer::from_identity(&identity, addrs).unwrap();
        assert_eq!(peer.max_scope(), Some(AddressScope::Public));
    }

    #[test]
    fn max_scope_private() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let addrs = vec![
            "/ip4/127.0.0.1/tcp/1234".parse().unwrap(),
            "/ip4/192.168.1.1/tcp/1234".parse().unwrap(),
        ];

        let peer = SwarmPeer::from_identity(&identity, addrs).unwrap();
        assert_eq!(peer.max_scope(), Some(AddressScope::Private));
    }

    #[test]
    fn max_scope_loopback_only() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let addrs = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        let peer = SwarmPeer::from_identity(&identity, addrs).unwrap();
        assert_eq!(peer.max_scope(), Some(AddressScope::Loopback));
    }

    #[test]
    fn has_scope_mixed() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let addrs = vec![
            "/ip4/127.0.0.1/tcp/1234".parse().unwrap(),
            "/ip4/192.168.1.1/tcp/1234".parse().unwrap(),
        ];

        let peer = SwarmPeer::from_identity(&identity, addrs).unwrap();
        assert!(peer.has_scope(AddressScope::Loopback));
        assert!(peer.has_scope(AddressScope::Private));
        assert!(!peer.has_scope(AddressScope::Public));
    }

    #[test]
    fn addrs_by_scope_filters() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let loopback: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let private: Multiaddr = "/ip4/192.168.1.1/tcp/1234".parse().unwrap();
        let public: Multiaddr = "/ip4/8.8.8.8/tcp/1234".parse().unwrap();
        let addrs = vec![loopback.clone(), private.clone(), public.clone()];

        let peer = SwarmPeer::from_identity(&identity, addrs).unwrap();

        let loopback_addrs = peer.addrs_by_scope(AddressScope::Loopback);
        assert_eq!(loopback_addrs.len(), 1);
        assert_eq!(loopback_addrs[0], loopback);

        let private_addrs = peer.addrs_by_scope(AddressScope::Private);
        assert_eq!(private_addrs.len(), 1);
        assert_eq!(private_addrs[0], private);

        let public_addrs = peer.addrs_by_scope(AddressScope::Public);
        assert_eq!(public_addrs.len(), 1);
        assert_eq!(public_addrs[0], public);
    }
}
