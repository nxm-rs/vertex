//! Traits and utilities for network primitives in the Ethereum Swarm P2P network.
//!
//! This crate provides the core traits and types needed for node addressing and
//! identification in the network. It defines the [`NodeAddress`] trait and related
//! functionality for managing node addresses across different network configurations.

use alloy_primitives::{Address, Keccak256, PrimitiveSignature, B256};
use libp2p::Multiaddr;
use nectar_primitives_traits::SwarmAddress;
use thiserror::Error;

/// Errors that can occur when working with node addresses.
#[derive(Error, Debug)]
pub enum NodeAddressError {
    /// Wrapper for signature validation errors from the alloy crate
    #[error("Invalid signature: {0}")]
    InvalidSignature(#[from] alloy_primitives::SignatureError),

    /// Wrapper for signer-related errors from the alloy crate
    #[error("Signer error: {0}")]
    SignerError(#[from] alloy_signer::Error),

    /// Indicates that the calculated overlay address doesn't match the expected value
    #[error("Invalid overlay address")]
    InvalidOverlay,
}

/// Defines the interface for node addresses in the network.
///
/// A node address consists of multiple components that together uniquely identify
/// a node in the network:
/// - An overlay address (derived from chain address and nonce)
/// - An underlay address (physical network address)
/// - A chain address (Ethereum address)
/// - A nonce value
/// - A signature proving ownership
///
/// The generic parameter `N` represents the network ID.
pub trait NodeAddress<const N: u64> {
    /// Calculates the overlay address for this node.
    ///
    /// The overlay address is derived from the chain address, network ID, and nonce
    /// using the Keccak256 hash function.
    fn overlay_address(&self) -> SwarmAddress {
        calculate_overlay_address::<N>(self.chain_address(), self.nonce())
    }

    /// Returns the underlay address (physical network address) of the node.
    fn underlay_address(&self) -> &Multiaddr;

    /// Returns the chain address (Ethereum address) of the node.
    fn chain_address(&self) -> &Address;

    /// Returns the nonce used in address generation.
    fn nonce(&self) -> &B256;

    /// Returns the signature proving ownership of the address.
    ///
    /// # Errors
    /// Returns a [`NodeAddressError`] if the signature is invalid or unavailable.
    fn signature(&self) -> Result<&PrimitiveSignature, NodeAddressError>;
}

/// Calculates the overlay address for a node given its chain address and nonce.
///
/// The overlay address is a Keccak256 hash of the concatenation of:
/// - The chain address
/// - The network ID (in little-endian bytes)
/// - The nonce
///
/// # Parameters
/// * `chain_address` - The Ethereum address of the node
/// * `nonce` - A unique nonce value
///
/// # Type Parameters
/// * `N` - The network ID
pub fn calculate_overlay_address<const N: u64>(
    chain_address: &Address,
    nonce: &B256,
) -> SwarmAddress {
    let mut hasher = Keccak256::new();
    hasher.update(chain_address);
    hasher.update(N.to_le_bytes());
    hasher.update(nonce);
    hasher.finalize()
}
