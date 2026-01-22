//! Implementation of network primitives for the Ethereum Swarm P2P network.
//!
//! This crate provides concrete implementations of the network primitive traits,
//! including a builder pattern for constructing node addresses and utilities for
//! address verification and signature handling.

use alloy_primitives::{Address, B256, Signature};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use bytes::{Bytes, BytesMut};
use std::io::{Cursor, Read};
use std::sync::Arc;
use std::net::{Ipv4Addr, Ipv6Addr};
use vertex_net_primitives_traits::{
    NodeAddress as NodeAddressTrait, NodeAddressError, calculate_overlay_address,
};

use libp2p::Multiaddr;
use nectar_primitives::SwarmAddress;

// =============================================================================
// Underlay Serialization (Bee-compatible)
// =============================================================================

/// Magic byte prefix for serialized lists of multiple underlays.
/// This value (0x99 = 153) is chosen because it's not a valid multiaddr protocol code,
/// ensuring legacy parsers will fail gracefully when encountering the new list format.
pub const UNDERLAY_LIST_PREFIX: u8 = 0x99;

/// Error type for underlay serialization/deserialization.
#[derive(Debug, thiserror::Error)]
pub enum UnderlayError {
    #[error("empty byte slice")]
    EmptyData,
    #[error("failed to read varint: {0}")]
    VarintError(#[from] std::io::Error),
    #[error("inconsistent data: expected {expected} bytes, got {actual}")]
    InconsistentLength { expected: u64, actual: usize },
    #[error("failed to parse multiaddr: {0}")]
    InvalidMultiaddr(#[from] libp2p::multiaddr::Error),
}

/// Serializes a slice of multiaddrs into a single byte slice.
///
/// This follows Bee's serialization format:
/// - If exactly one address: returns the raw multiaddr bytes (backward compatible)
/// - If zero or 2+ addresses: prefixes with 0x99 magic byte, then varint-length-prefixed addresses
pub fn serialize_underlays(addrs: &[Multiaddr]) -> Vec<u8> {
    // Backward compatibility: single address is just raw bytes
    if addrs.len() == 1 {
        return addrs[0].to_vec();
    }

    // For 0 or 2+ addresses, use the list format with prefix
    let mut buf = Vec::new();
    buf.push(UNDERLAY_LIST_PREFIX);

    for addr in addrs {
        let addr_bytes = addr.to_vec();
        // Write varint-encoded length
        buf.extend(encode_uvarint(addr_bytes.len() as u64));
        buf.extend(addr_bytes);
    }

    buf
}

/// Deserializes a byte slice into a vector of multiaddrs.
///
/// Automatically detects the format:
/// - If starts with 0x99: parses as a list of varint-length-prefixed addresses
/// - Otherwise: parses as a single legacy multiaddr
pub fn deserialize_underlays(data: &[u8]) -> Result<Vec<Multiaddr>, UnderlayError> {
    if data.is_empty() {
        return Err(UnderlayError::EmptyData);
    }

    // Check for list format (magic prefix)
    if data[0] == UNDERLAY_LIST_PREFIX {
        return deserialize_underlay_list(&data[1..]);
    }

    // Legacy format: single multiaddr
    let addr = Multiaddr::try_from(data.to_vec())?;
    Ok(vec![addr])
}

/// Deserializes the list format (after the magic prefix has been stripped).
fn deserialize_underlay_list(data: &[u8]) -> Result<Vec<Multiaddr>, UnderlayError> {
    let mut addrs = Vec::new();
    let mut cursor = Cursor::new(data);

    while (cursor.position() as usize) < data.len() {
        // Read varint-encoded length
        let addr_len = decode_uvarint(&mut cursor)?;

        // Check we have enough bytes
        let remaining = data.len() - cursor.position() as usize;
        if (addr_len as usize) > remaining {
            return Err(UnderlayError::InconsistentLength {
                expected: addr_len,
                actual: remaining,
            });
        }

        // Read address bytes
        let mut addr_bytes = vec![0u8; addr_len as usize];
        cursor.read_exact(&mut addr_bytes)?;

        // Parse multiaddr
        let addr = Multiaddr::try_from(addr_bytes)?;
        addrs.push(addr);
    }

    Ok(addrs)
}

/// Encode a u64 as an unsigned varint (LEB128).
fn encode_uvarint(mut value: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80; // Set continuation bit
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
    buf
}

/// Decode an unsigned varint (LEB128) from a cursor.
fn decode_uvarint(cursor: &mut Cursor<&[u8]>) -> Result<u64, std::io::Error> {
    let mut result: u64 = 0;
    let mut shift = 0;

    loop {
        let mut byte = [0u8; 1];
        cursor.read_exact(&mut byte)?;
        let b = byte[0];

        result |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "varint too long",
            ));
        }
    }

    Ok(result)
}

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
    pub fn with_signer<S: alloy_signer::Signer + SignerSync>(
        self,
        signer: Arc<S>,
    ) -> Result<NodeAddressBuilder<ReadyToBuild>, NodeAddressError> {
        let network_id = self.network_id.unwrap();
        let nonce = self.nonce.as_ref().unwrap();
        let underlay = self.underlay.as_ref().unwrap();

        let overlay = calculate_overlay_address(&signer.address(), network_id, nonce);
        // For local signing, use the single underlay's raw bytes
        let underlay_bytes = underlay.to_vec();
        let msg = generate_sign_message(&underlay_bytes, &overlay, network_id);
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

    /// Verify a signature from a remote peer.
    ///
    /// `underlay_bytes_for_sig` should be the raw serialized underlay bytes as received
    /// over the wire (may include 0x99 prefix for multiple underlays).
    pub fn with_signature(
        self,
        underlay_bytes_for_sig: &[u8],
        overlay: &SwarmAddress,
        signature: Signature,
        verify_overlay: bool,
    ) -> Result<NodeAddressBuilder<ReadyToBuild>, NodeAddressError> {
        let network_id = self.network_id.unwrap();
        let nonce = self.nonce.as_ref().unwrap();

        let chain_address = recover_signer(underlay_bytes_for_sig, overlay, &signature, network_id)?;

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
/// - The underlay address bytes (raw serialized bytes, may include 0x99 prefix for multiple)
/// - The overlay address bytes
/// - The network ID in big-endian bytes
pub fn generate_sign_message(underlay_bytes: &[u8], overlay: &SwarmAddress, network_id: u64) -> Bytes {
    let mut message = BytesMut::new();
    message.extend_from_slice(b"bee-handshake-");
    message.extend_from_slice(underlay_bytes);
    message.extend_from_slice(overlay.as_ref());
    message.extend_from_slice(network_id.to_be_bytes().as_slice());
    message.freeze()
}

/// Recovers the signer's address from a signature and message components.
///
/// # Errors
/// Returns a [`NodeAddressError`] if signature recovery fails.
pub fn recover_signer(
    underlay_bytes: &[u8],
    overlay: &SwarmAddress,
    signature: &Signature,
    network_id: u64,
) -> Result<Address, NodeAddressError> {
    let prehash = generate_sign_message(underlay_bytes, overlay, network_id);
    Ok(signature.recover_address_from_msg(prehash)?)
}

/// Validates a BzzAddress by verifying the signature and overlay derivation.
///
/// This function performs the following checks:
/// 1. Recovers the Ethereum address from the signature
/// 2. Computes the expected overlay address from the recovered address, network_id, and nonce
/// 3. Verifies the computed overlay matches the claimed overlay
pub fn validate_bzz_address(
    underlays: &[Multiaddr],
    overlay: &B256,
    signature: &Signature,
    nonce: &B256,
    network_id: u64,
) -> Result<(), NodeAddressError> {
    let underlay_bytes = serialize_underlays(underlays);
    let overlay_addr = SwarmAddress::from(*overlay);

    let recovered_address = recover_signer(&underlay_bytes, &overlay_addr, signature, network_id)?;
    let expected_overlay = calculate_overlay_address(&recovered_address, network_id, nonce);

    if expected_overlay != overlay_addr {
        return Err(NodeAddressError::InvalidOverlay);
    }

    Ok(())
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
        // For tests, single underlay = raw bytes
        let underlay_bytes = underlay.to_vec();
        NodeAddress::builder()
            .with_network_id(network_id)
            .with_nonce(nonce)
            .with_underlay(underlay)
            .with_signature(&underlay_bytes, overlay, signature, true)
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
            let underlay_bytes = node.underlay_address().to_vec();
            let recovered_address = recover_signer(
                &underlay_bytes,
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
