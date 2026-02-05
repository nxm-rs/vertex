//! Peer identity and addressing primitives for the Ethereum Swarm P2P network.
//!
//! - [`SwarmPeer`] - Canonical peer identity type
//! - [`SwarmIdentityExt`] - Extension trait for creating peers from identities
//! - Multiaddr serialization (Bee-compatible)
//! - Signature verification and overlay validation

mod error;
mod serde_multiaddr;
mod util;

pub use error::{MultiAddrError, SwarmPeerError};
pub use serde_multiaddr::{deserialize_multiaddrs, serialize_multiaddrs};
pub use util::arbitrary_multiaddr;

use util::generate_sign_message;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_primitives::compute_overlay;
use vertex_swarm_spec::SwarmSpec;

pub use nectar_primitives::SwarmAddress;
pub use nectar_swarms::{NamedSwarm, Swarm, SwarmKind};
pub use vertex_swarm_primitives::SwarmNodeType;

use alloy_primitives::{Address, B256, Signature};
use alloy_signer::{Signer, SignerSync};
use libp2p::Multiaddr;

/// Extension trait for creating [`SwarmPeer`] from any [`SwarmIdentity`].
pub trait SwarmIdentityExt: SwarmIdentity {
    /// Create a [`SwarmPeer`] from this identity with the given multiaddrs.
    fn to_swarm_peer(&self, multiaddrs: Vec<Multiaddr>) -> Result<SwarmPeer, SwarmPeerError> {
        SwarmPeer::from_identity(self, multiaddrs)
    }
}

impl<I: SwarmIdentity> SwarmIdentityExt for I {}

/// Verifiable peer identity with multiaddrs, signature, and overlay address.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SwarmPeer {
    multiaddrs: Vec<Multiaddr>,
    signature: Signature,
    overlay: SwarmAddress,
    nonce: B256,
    ethereum_address: Address,
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

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_identity::Identity;
    use vertex_swarm_primitives::SwarmNodeType;
    use vertex_swarm_spec::{init_testnet, SpecBuilder};

    #[test]
    fn swarm_peer_roundtrip() {
        let spec = init_testnet();
        let identity = Identity::random((*spec).clone(), SwarmNodeType::Storer);
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
        let identity = Identity::random((*spec).clone(), SwarmNodeType::Storer);
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
        let identity1 = Identity::random((*spec).clone(), SwarmNodeType::Storer);
        let identity2 = Identity::random((*spec).clone(), SwarmNodeType::Storer);
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

        let identity1 = Identity::random(spec1, SwarmNodeType::Storer);
        let identity2 = Identity::random(spec2, SwarmNodeType::Storer);
        let identity3 = Identity::random(spec3, SwarmNodeType::Storer);

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
        let identity = Identity::random((*spec).clone(), SwarmNodeType::Storer);

        let result = SwarmPeer::from_identity(&identity, vec![]);
        assert!(matches!(result, Err(SwarmPeerError::NoMultiaddrs)));
    }

    #[test]
    fn empty_multiaddrs_rejected_from_signed() {
        let spec = init_testnet();
        let identity = Identity::random((*spec).clone(), SwarmNodeType::Storer);
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
}
