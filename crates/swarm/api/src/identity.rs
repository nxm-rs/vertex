//! Identity trait for Swarm network participation.

use alloy_primitives::{Address, B256};
use alloy_signer::{Signer, SignerSync};
use nectar_primitives::SwarmAddress;
use std::sync::Arc;
use vertex_swarm_primitives::{SwarmNodeType, compute_overlay};
use vertex_swarmspec::SwarmSpec;

/// Identity trait for Swarm network participation.
///
/// Provides cryptographic identity for handshake and overlay address derivation.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmIdentity: Clone + Send + Sync + 'static {
    /// The network specification type.
    type Spec: SwarmSpec + Clone;

    /// The signer type for signing handshake messages.
    type Signer: Signer + SignerSync + Clone + Send + Sync + 'static;

    /// Get the network specification.
    fn spec(&self) -> &Self::Spec;

    /// Get the nonce for overlay address derivation.
    fn nonce(&self) -> B256;

    /// Get the signer for handshake authentication.
    fn signer(&self) -> Arc<Self::Signer>;

    /// The node type (capability level).
    fn node_type(&self) -> SwarmNodeType;

    /// Overlay address for Kademlia routing.
    ///
    /// Default computes on every call. Override to return cached value.
    fn overlay_address(&self) -> SwarmAddress {
        compute_overlay(
            &self.ethereum_address(),
            self.spec().network_id(),
            &self.nonce(),
        )
    }

    /// Ethereum address derived from the signing key.
    fn ethereum_address(&self) -> Address {
        self.signer().address()
    }

    /// Whether this node operates as a full node (stores chunks).
    fn is_full_node(&self) -> bool {
        self.node_type().requires_storage()
    }

    /// Optional welcome message for peers (max 140 chars).
    fn welcome_message(&self) -> Option<&str> {
        Some("Buzzing in from the Rustacean hive")
    }
}
