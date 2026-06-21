//! Identity trait for Swarm network participation.

use crate::SwarmSpec;
use alloy_primitives::Address;
use alloy_signer::{Signer, SignerSync};
use nectar_primitives::SwarmAddress;
use std::sync::Arc;
use vertex_swarm_primitives::{OverlaySigner, SwarmNodeType};

/// Identity trait for Swarm network participation.
///
/// The signing and overlay-derivation facet is the [`OverlaySigner`] supertrait;
/// this trait adds the spec, concrete signer handle, and node role. The two
/// associated types (`Spec`, `Signer`) are what keep it from being object-safe,
/// so a consumer that needs an erased handle depends on `OverlaySigner` instead.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmIdentity: OverlaySigner + Send + Sync + 'static {
    /// The network specification type.
    type Spec: SwarmSpec;

    /// The signer type for signing handshake messages.
    type Signer: Signer + SignerSync + Send + Sync + 'static;

    /// Get the network specification.
    fn spec(&self) -> &Self::Spec;

    /// Get the signer for handshake authentication.
    fn signer(&self) -> Arc<Self::Signer>;

    /// The node type (capability level).
    fn node_type(&self) -> SwarmNodeType;

    /// Overlay address for Kademlia routing (the [`OverlaySigner`] facet).
    fn overlay_address(&self) -> SwarmAddress {
        self.overlay()
    }

    /// Ethereum address derived from the signing key (the [`OverlaySigner`] facet).
    fn ethereum_address(&self) -> Address {
        self.address()
    }

    /// Whether this node is a Storer (stores chunks).
    ///
    /// Storers announce the storer flag in the handshake and are the only
    /// node type gossiped network-wide.
    fn is_storer(&self) -> bool {
        self.node_type().requires_storage()
    }

    /// Optional welcome message for peers (max 140 chars).
    fn welcome_message(&self) -> Option<&str> {
        Some("Buzzing in from the Rustacean hive")
    }
}
