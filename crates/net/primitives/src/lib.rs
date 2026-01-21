//! Implementation of network primitives for the Ethereum Swarm P2P network.
//!
//! This crate provides concrete implementations of the network primitive traits,
//! including a builder pattern for constructing node addresses and utilities for
//! address verification and signature handling.

use alloy_primitives::{Address, B256, Signature};
use alloy_signer::{SignerSync, k256::ecdsa::SigningKey};
use alloy_signer_local::{LocalSigner, PrivateKeySigner};
use bytes::{Bytes, BytesMut};
use std::sync::Arc;
use std::net::{Ipv4Addr, Ipv6Addr};
use vertex_net_primitives_traits::{
    NodeAddress as NodeAddressTrait, NodeAddressError, calculate_overlay_address,
};

use libp2p::Multiaddr;
use nectar_primitives::SwarmAddress;

// Re-export swarm network types from nectar-swarms
pub use nectar_swarms::{NamedSwarm, Swarm, SwarmKind};

/// Represents a complete node address in the network.
///
/// Contains all components needed to identify and communicate with a node.
#[derive(Debug, Clone, Eq)]
pub struct NodeAddress {
    network_id: u64,
    nonce: B256,
    underlay_address: Multiaddr,
    chain_address: Address,
    signature: Signature,
}

/// Builder pattern states for NodeAddress construction.
/// These empty traits are used for type-state pattern implementation.
pub trait BuilderState {}
pub struct Initial;
pub struct WithNetworkId;
pub struct WithNonce;
pub struct WithUnderlay;
pub struct ReadyToBuild;
impl BuilderState for Initial {}
impl BuilderState for WithNetworkId {}
impl BuilderState for WithNonce {}
impl BuilderState for WithUnderlay {}
impl BuilderState for ReadyToBuild {}

impl PartialEq for NodeAddress {
    fn eq(&self, other: &Self) -> bool {
        self.network_id == other.network_id && self.overlay_address() == other.overlay_address()
    }
}

impl NodeAddress {
    pub fn builder() -> NodeAddressBuilder<Initial> {
        NodeAddressBuilder::default()
    }
}

impl NodeAddressTrait for NodeAddress {
    fn network_id(&self) -> u64 {
        self.network_id
    }

    fn underlay_address(&self) -> &Multiaddr {
        &self.underlay_address
    }

    fn chain_address(&self) -> &Address {
        &self.chain_address
    }

    fn nonce(&self) -> &B256 {
        &self.nonce
    }

    fn signature(&self) -> Result<&Signature, NodeAddressError> {
        Ok(&self.signature)
    }
}

/// Builder for constructing NodeAddress instances in a type-safe manner.
///
/// The type parameter `State` tracks the building progress using the type-state pattern,
/// ensuring that required fields are set in the correct order.
#[derive(Debug)]
pub struct NodeAddressBuilder<State: BuilderState> {
    network_id: Option<u64>,
    nonce: Option<B256>,
    underlay: Option<Multiaddr>,
    chain_address: Option<Address>,
    signature: Option<Signature>,
    _state: std::marker::PhantomData<State>,
}

// Default implementation for all builder states
impl<State: BuilderState> Default for NodeAddressBuilder<State> {
    fn default() -> Self {
        Self {
            network_id: None,
            nonce: None,
            underlay: None,
            chain_address: None,
            signature: None,
            _state: std::marker::PhantomData,
        }
    }
}

impl NodeAddressBuilder<Initial> {
    pub fn with_network_id(self, network_id: u64) -> NodeAddressBuilder<WithNetworkId> {
        NodeAddressBuilder {
            network_id: Some(network_id),
            ..Default::default()
        }
    }
}

impl NodeAddressBuilder<WithNetworkId> {
    pub fn with_nonce(self, nonce: B256) -> NodeAddressBuilder<WithNonce> {
        NodeAddressBuilder {
            network_id: self.network_id,
            nonce: Some(nonce),
            ..Default::default()
        }
    }
}

impl NodeAddressBuilder<WithNonce> {
    pub fn with_underlay(self, underlay: Multiaddr) -> NodeAddressBuilder<WithUnderlay> {
        NodeAddressBuilder {
            network_id: self.network_id,
            nonce: self.nonce,
            underlay: Some(underlay),
            ..Default::default()
        }
    }
}

impl NodeAddressBuilder<WithUnderlay> {
    pub fn with_signer(
        self,
        signer: Arc<LocalSigner<SigningKey>>,
    ) -> Result<NodeAddressBuilder<ReadyToBuild>, NodeAddressError> {
        let network_id = self.network_id.unwrap();
        let nonce = self.nonce.as_ref().unwrap();
        let underlay = self.underlay.as_ref().unwrap();

        let overlay = calculate_overlay_address(&signer.address(), network_id, nonce);
        let msg = generate_sign_message(underlay, &overlay, network_id);
        let signature = signer
            .sign_message_sync(&msg)
            .map_err(NodeAddressError::SignerError)?;

        Ok(NodeAddressBuilder {
            network_id: self.network_id,
            nonce: self.nonce,
            underlay: self.underlay,
            chain_address: Some(signer.address()),
            signature: Some(signature),
            ..Default::default()
        })
    }

    pub fn with_signature(
        self,
        overlay: &SwarmAddress,
        signature: Signature,
        verify_overlay: bool,
    ) -> Result<NodeAddressBuilder<ReadyToBuild>, NodeAddressError> {
        let network_id = self.network_id.unwrap();
        let nonce = self.nonce.as_ref().unwrap();
        let underlay = self.underlay.as_ref().unwrap();

        let chain_address = recover_signer(underlay, overlay, &signature, network_id)?;

        if verify_overlay {
            let recovered_overlay = calculate_overlay_address(&chain_address, network_id, nonce);
            if &recovered_overlay != overlay {
                return Err(NodeAddressError::InvalidOverlay);
            }
        }

        Ok(NodeAddressBuilder {
            network_id: self.network_id,
            nonce: self.nonce,
            underlay: self.underlay,
            chain_address: Some(chain_address),
            signature: Some(signature),
            _state: std::marker::PhantomData,
        })
    }
}

impl NodeAddressBuilder<ReadyToBuild> {
    pub fn build(self) -> NodeAddress {
        NodeAddress {
            network_id: self.network_id.unwrap(),
            nonce: self.nonce.unwrap(),
            underlay_address: self.underlay.unwrap(),
            chain_address: self.chain_address.unwrap(),
            signature: self.signature.unwrap(),
        }
    }
}

/// Generates a message to be signed for node address verification.
///
/// The message consists of:
/// - A prefix ("bee-handshake-")
/// - The underlay address bytes
/// - The overlay address bytes
/// - The network ID in big-endian bytes
fn generate_sign_message(underlay: &Multiaddr, overlay: &SwarmAddress, network_id: u64) -> Bytes {
    let mut message = BytesMut::new();
    message.extend_from_slice(b"bee-handshake-");
    message.extend_from_slice(underlay.as_ref());
    message.extend_from_slice(overlay.as_ref());
    message.extend_from_slice(network_id.to_be_bytes().as_slice());
    message.freeze()
}

/// Recovers the signer's address from a signature and message components.
///
/// # Errors
/// Returns a [`NodeAddressError`] if signature recovery fails.
fn recover_signer(
    underlay: &Multiaddr,
    overlay: &SwarmAddress,
    signature: &Signature,
    network_id: u64,
) -> Result<Address, NodeAddressError> {
    let prehash = generate_sign_message(underlay, overlay, network_id);
    Ok(signature.recover_address_from_msg(prehash)?)
}

impl<'a> arbitrary::Arbitrary<'a> for NodeAddress {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let network_id: u64 = u.arbitrary()?;
        let nonce = u.arbitrary()?;
        let underlay_address = arbitrary_multiaddr(u)?;
        let signer = Arc::new(PrivateKeySigner::random());

        Ok(NodeAddress::builder()
            .with_network_id(network_id)
            .with_nonce(nonce)
            .with_underlay(underlay_address)
            .with_signer(signer)
            .map_err(|_| arbitrary::Error::IncorrectFormat)?
            .build())
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;

    const TEST_NETWORK_ID: u64 = 1;

    /// Helper function to create a node address with a signer
    fn create_node_with_signer(
        network_id: u64,
        nonce: B256,
        underlay: Multiaddr,
        signer: Arc<PrivateKeySigner>,
    ) -> Result<NodeAddress, NodeAddressError> {
        NodeAddress::builder()
            .with_network_id(network_id)
            .with_nonce(nonce)
            .with_underlay(underlay)
            .with_signer(signer)
            .map(|builder| builder.build())
    }

    /// Helper function to create a node address from existing signature
    fn create_node_from_signature(
        network_id: u64,
        nonce: B256,
        underlay: Multiaddr,
        overlay: &SwarmAddress,
        signature: Signature,
    ) -> Result<NodeAddress, NodeAddressError> {
        NodeAddress::builder()
            .with_network_id(network_id)
            .with_nonce(nonce)
            .with_underlay(underlay)
            .with_signature(overlay, signature, true)
            .map(|builder| builder.build())
    }

    proptest! {
        #[test]
        fn test_node_address_roundtrip(
            node in arb::<NodeAddress>()
        ) {
            // Calculate overlay address from the original node
            let overlay = calculate_overlay_address(
                node.chain_address(),
                node.network_id(),
                node.nonce()
            );

            // Create a new node using the signature from the first node
            let reconstructed_node = create_node_from_signature(
                node.network_id(),
                node.nonce().clone(),
                node.underlay_address().clone(),
                &overlay,
                node.signature().unwrap().clone()
            );

            prop_assert!(reconstructed_node.is_ok());
            let reconstructed_node = reconstructed_node.unwrap();

            // Verify that both nodes are equal (they should have the same overlay address)
            prop_assert_eq!(&node, &reconstructed_node);

            // Verify all individual fields match
            prop_assert_eq!(node.network_id(), reconstructed_node.network_id());
            prop_assert_eq!(node.nonce(), reconstructed_node.nonce());
            prop_assert_eq!(node.underlay_address(), reconstructed_node.underlay_address());
            prop_assert_eq!(node.chain_address(), reconstructed_node.chain_address());
            prop_assert_eq!(node.signature().unwrap(), reconstructed_node.signature().unwrap());
        }

        #[test]
        fn test_node_address_properties(node in arb::<NodeAddress>()) {
            // Test basic properties
            let overlay = calculate_overlay_address(
                node.chain_address(),
                node.network_id(),
                node.nonce()
            );

            // Verify signature recovery
            let recovered_address = recover_signer(
                node.underlay_address(),
                &overlay,
                node.signature().unwrap(),
                node.network_id()
            );

            prop_assert!(recovered_address.is_ok());
            prop_assert_eq!(recovered_address.unwrap(), *node.chain_address());

            // Verify multiaddr format
            let addr_str = node.underlay_address().to_string();
            prop_assert!(addr_str.contains("/ip4/") || addr_str.contains("/ip6/"));
            prop_assert!(addr_str.contains("/tcp/"));
        }
    }

    #[test]
    fn test_explicit_node_address_creation() {
        let signer = Arc::new(PrivateKeySigner::random());
        let nonce = B256::ZERO;
        let underlay: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        // Create first node with signer
        let node1 = create_node_with_signer(
            TEST_NETWORK_ID,
            nonce.clone(),
            underlay.clone(),
            signer.clone(),
        )
        .expect("Should create node1");

        // Calculate overlay address
        let overlay = node1.overlay_address();

        // Create second node using signature from first node
        let node2 = create_node_from_signature(
            TEST_NETWORK_ID,
            nonce,
            underlay,
            &overlay,
            node1.signature().unwrap().clone(),
        )
        .expect("Should create node2");

        // Verify equality
        assert_eq!(node1, node2);
        assert_eq!(node1.chain_address(), node2.chain_address());
        assert_eq!(node1.overlay_address(), node2.overlay_address());
    }

    #[test]
    fn test_invalid_signature_verification() {
        let signer1 = Arc::new(PrivateKeySigner::random());
        let signer2 = Arc::new(PrivateKeySigner::random());
        let nonce = B256::ZERO;
        let underlay: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        // Create node with first signer
        let node1 = create_node_with_signer(
            TEST_NETWORK_ID,
            nonce.clone(),
            underlay.clone(),
            signer1,
        )
        .expect("Should create node1");

        // Calculate overlay address for second signer
        let overlay2 = calculate_overlay_address(&signer2.address(), TEST_NETWORK_ID, &nonce);

        // Attempt to create node with mismatched signature and overlay
        let result = create_node_from_signature(
            TEST_NETWORK_ID,
            nonce,
            underlay,
            &overlay2,
            node1.signature().unwrap().clone(),
        );

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            NodeAddressError::InvalidOverlay
        ));
    }

    #[test]
    fn test_different_networks_generate_different_overlays() {
        // Create a node address with the same parameters but different network IDs
        let signer = Arc::new(PrivateKeySigner::random());
        let nonce = B256::ZERO;
        let underlay: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        // Create nodes for different networks
        let node1 = create_node_with_signer(1, nonce.clone(), underlay.clone(), signer.clone())
            .expect("Should create node1");

        let node2 = create_node_with_signer(2, nonce.clone(), underlay.clone(), signer.clone())
            .expect("Should create node2");

        let node3 = create_node_with_signer(100, nonce, underlay, signer)
            .expect("Should create node3");

        // Calculate overlay addresses for each network
        let overlay1 = node1.overlay_address();
        let overlay2 = node2.overlay_address();
        let overlay3 = node3.overlay_address();

        // Verify that all overlay addresses are different
        assert_ne!(overlay1, overlay2);
        assert_ne!(overlay1, overlay3);
        assert_ne!(overlay2, overlay3);

        // Extra verification: signatures should not be equal
        assert_ne!(node1.signature().unwrap(), node2.signature().unwrap());
        assert_ne!(node1.signature().unwrap(), node3.signature().unwrap());
        assert_ne!(node2.signature().unwrap(), node3.signature().unwrap());
    }
}
