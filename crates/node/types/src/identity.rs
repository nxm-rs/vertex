//! Identity trait for Swarm network participation.
//!
//! The [`Identity`] trait defines the interface that the network layer needs
//! from a node's cryptographic identity. This allows the network protocols
//! (handshake, etc.) to be generic over the identity implementation.
//!
//! # Required Capabilities
//!
//! - Network specification (which swarm to join, via associated `Spec` type)
//! - Cryptographic signing (for handshake authentication)
//! - Overlay address derivation (for routing)
//! - Node metadata (full node status, welcome message)
//!
//! # Associated Types
//!
//! - `Spec`: The network specification (provides network_id, etc.)
//! - `Signer`: The signing backend for handshake authentication

use alloy_primitives::{Address, B256};
use alloy_signer::{Signer, SignerSync};
use core::fmt::Debug;
use nectar_primitives::SwarmAddress;
use vertex_net_primitives_traits::calculate_overlay_address;
use vertex_swarmspec::SwarmSpec;

/// Trait defining the identity interface for Swarm network participation.
///
/// This trait abstracts the identity capabilities needed by the network layer,
/// allowing protocols like handshake to work with any identity implementation.
///
/// # Example Implementation
///
/// ```ignore
/// use vertex_node_types::Identity;
///
/// struct MyIdentity {
///     spec: Arc<Hive>,
///     signer: Arc<MySigner>,
///     nonce: B256,
///     // ...
/// }
///
/// impl Identity for MyIdentity {
///     type Spec = Hive;
///     type Signer = MySigner;
///
///     fn spec(&self) -> &Self::Spec { &self.spec }
///     fn nonce(&self) -> B256 { self.nonce }
///     // ...
/// }
/// ```
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait Identity: Clone + Debug + Send + Sync + 'static {
    /// The network specification type.
    ///
    /// Provides network identity (mainnet, testnet, etc.), network_id,
    /// and other network-specific configuration.
    type Spec: SwarmSpec + Clone;

    /// The signer type used for signing handshake messages.
    ///
    /// Must implement `alloy_signer::Signer` for the default `ethereum_address`
    /// implementation and `SignerSync` for synchronous signing in handshake.
    type Signer: Signer + SignerSync + Clone + Send + Sync + 'static;

    /// Get the network specification.
    ///
    /// The spec provides access to network_id and other network configuration.
    fn spec(&self) -> &Self::Spec;

    /// Get the nonce used for overlay address derivation.
    ///
    /// The nonce allows the same Ethereum key to have different overlay addresses,
    /// which is useful for testing or running multiple nodes with the same key.
    fn nonce(&self) -> B256;

    /// Get the signer for signing handshake messages.
    ///
    /// Returns an Arc-wrapped signer for efficient cloning and sharing.
    fn signer(&self) -> alloc::sync::Arc<Self::Signer>;

    /// Check if this node operates as a full node.
    ///
    /// Full nodes store chunks and participate in the storage incentive.
    /// Light nodes only retrieve data and don't store chunks for others.
    fn is_full_node(&self) -> bool;

    /// Get the optional welcome message for peers.
    ///
    /// This message is exchanged during handshake and can be used
    /// to identify the node (e.g., operator name, version info).
    /// Maximum length is typically 140 characters.
    ///
    /// Default: A friendly Rustacean bee greeting.
    fn welcome_message(&self) -> Option<&str> {
        Some("Buzzing in from the Rustacean hive")
    }

    /// Get the Ethereum address derived from the signing key.
    ///
    /// This is the address that identifies this node on the Ethereum network
    /// and is used for SWAP payments.
    ///
    /// Default implementation calls `self.signer().address()`.
    fn ethereum_address(&self) -> Address {
        self.signer().address()
    }

    /// Get the overlay address for this identity.
    ///
    /// The overlay address determines the node's position in the
    /// Kademlia routing table and which chunks it's responsible for.
    ///
    /// Computed as: `keccak256(ethereum_address || network_id || nonce)`
    /// where network_id comes from `self.spec().network_id()`.
    fn overlay_address(&self) -> SwarmAddress {
        calculate_overlay_address(
            &self.ethereum_address(),
            self.spec().network_id(),
            &self.nonce(),
        )
    }
}
