//! Swarm network identity implementation for Vertex nodes.
//!
//! A node's identity consists of:
//! - A signing key (Ethereum keypair) - loaded from standard Ethereum keystore
//! - A nonce - stored in config, used to derive different overlay addresses
//! - Network specification - determines which network to join
//!
//! The overlay address is derived as: `keccak256(eth_address || network_id || nonce)`
//!
//! This allows the same Ethereum key to have different overlay addresses by
//! changing the nonce, which is useful for testing or running multiple nodes.
//!
//! # Architecture
//!
//! This crate provides [`SwarmIdentity`], the standard implementation of the
//! [`Identity`] trait from `vertex-node-types`. The trait defines the interface
//! that the network layer needs from a node's cryptographic identity.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

use alloy_primitives::B256;
use alloy_signer::k256::ecdsa::SigningKey;
use alloy_signer_local::LocalSigner;
use nectar_primitives::SwarmAddress;
use std::sync::Arc;
use vertex_net_primitives_traits::calculate_overlay_address;
use vertex_swarm_api::Identity;
use vertex_swarmspec::{Hive, SwarmSpec};

// Re-export the Identity trait for convenience
pub use vertex_swarm_api::Identity as IdentityTrait;

/// Standard identity implementation for Swarm nodes.
///
/// Contains everything needed for the handshake protocol:
/// network identification, signing capability, and node metadata.
///
/// This is the core cryptographic identity of the node, used for:
/// - Signing handshake messages
/// - Deriving the overlay address
/// - Participating in the Swarm network
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_identity::SwarmIdentity;
/// use vertex_swarmspec::init_mainnet;
///
/// // Load signer from keystore
/// let signer = LocalSigner::decrypt_keystore(&keystore_path, &password)?;
///
/// // Get nonce from config (or generate one)
/// let nonce = config.identity.nonce.unwrap_or_else(|| B256::random());
///
/// // Create identity
/// let spec = init_mainnet();
/// let identity = SwarmIdentity::new(signer, nonce, spec, true);
/// ```
#[derive(Clone)]
pub struct SwarmIdentity {
    /// The network specification (contains network_id, bootnodes, etc.)
    spec: Arc<Hive>,

    /// The signing key for this node.
    signer: Arc<LocalSigner<SigningKey>>,

    /// The nonce used for overlay address derivation.
    nonce: B256,

    /// Cached overlay address (computed once at construction).
    overlay: SwarmAddress,

    /// Whether this node operates as a full node.
    is_full_node: bool,

    /// Optional custom welcome message for peers.
    /// If `None`, uses the default from the Identity trait.
    welcome_message: Option<String>,
}

impl std::fmt::Debug for SwarmIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwarmIdentity")
            .field("ethereum_address", &self.ethereum_address())
            .field("overlay_address", &self.overlay_address())
            .field("network_id", &self.spec.network_id())
            .field("network_name", &self.spec.network_name())
            .field("is_full_node", &self.is_full_node)
            .finish_non_exhaustive()
    }
}

impl SwarmIdentity {
    /// Create a new swarm identity.
    ///
    /// # Arguments
    ///
    /// * `signer` - The signing key, typically loaded from an Ethereum keystore
    /// * `nonce` - The nonce for overlay address derivation (from config)
    /// * `spec` - The network specification
    /// * `is_full_node` - Whether this node stores chunks for the network
    pub fn new(
        signer: LocalSigner<SigningKey>,
        nonce: B256,
        spec: Arc<Hive>,
        is_full_node: bool,
    ) -> Self {
        let overlay = calculate_overlay_address(&signer.address(), spec.network_id(), &nonce);
        Self {
            spec,
            signer: Arc::new(signer),
            nonce,
            overlay,
            is_full_node,
            welcome_message: None,
        }
    }

    /// Create a random ephemeral identity for testing or light nodes.
    ///
    /// Generates a random signing key and nonce. The identity is not persisted.
    pub fn random(spec: Arc<Hive>, is_full_node: bool) -> Self {
        use rand::Rng;
        let mut rng = rand::rng();

        // Generate random signing key using the correct rand version
        let mut key_bytes = [0u8; 32];
        rng.fill(&mut key_bytes);
        let signing_key =
            SigningKey::from_slice(&key_bytes).expect("32 bytes is valid for secp256k1");
        let signer = LocalSigner::from_signing_key(signing_key);

        // Generate random nonce
        let mut nonce_bytes = [0u8; 32];
        rng.fill(&mut nonce_bytes);
        let nonce = B256::from(nonce_bytes);

        Self::new(signer, nonce, spec, is_full_node)
    }

    /// Set the welcome message.
    pub fn with_welcome_message(mut self, message: impl Into<String>) -> Self {
        self.welcome_message = Some(message.into());
        self
    }
}

impl Identity for SwarmIdentity {
    type Spec = Hive;
    type Signer = LocalSigner<SigningKey>;

    fn spec(&self) -> &Self::Spec {
        &self.spec
    }

    fn nonce(&self) -> B256 {
        self.nonce
    }

    fn signer(&self) -> Arc<Self::Signer> {
        self.signer.clone()
    }

    fn is_full_node(&self) -> bool {
        self.is_full_node
    }

    fn welcome_message(&self) -> Option<&str> {
        self.welcome_message
            .as_deref()
            .or(Some("Buzzing in from the Rustacean hive"))
    }

    fn overlay_address(&self) -> SwarmAddress {
        self.overlay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarmspec::init_testnet;

    #[test]
    fn test_random_identity() {
        let spec = init_testnet();
        let identity = SwarmIdentity::random(spec, true);

        // Should have valid addresses
        assert!(!identity.ethereum_address().is_zero());
        assert!(!identity.overlay_address().is_zero());
        assert!(identity.is_full_node());
    }

    #[test]
    fn test_identity_with_known_nonce() {
        let spec = init_testnet();

        let mut rng = rand::rng();
        let mut key_bytes = [0u8; 32];
        rand::Rng::fill(&mut rng, &mut key_bytes);
        let signing_key = SigningKey::from_slice(&key_bytes).unwrap();
        let signer = LocalSigner::from_signing_key(signing_key);
        let nonce = B256::from([0x42u8; 32]);

        let identity1 = SwarmIdentity::new(signer.clone(), nonce, spec.clone(), true);
        let identity2 = SwarmIdentity::new(signer, nonce, spec, true);

        // Same signer + nonce = same overlay address
        assert_eq!(identity1.overlay_address(), identity2.overlay_address());
        assert_eq!(identity1.ethereum_address(), identity2.ethereum_address());
    }

    #[test]
    fn test_different_nonce_different_overlay() {
        let spec = init_testnet();

        let mut rng = rand::rng();
        let mut key_bytes = [0u8; 32];
        rand::Rng::fill(&mut rng, &mut key_bytes);
        let signing_key = SigningKey::from_slice(&key_bytes).unwrap();
        let signer = LocalSigner::from_signing_key(signing_key);

        let identity1 =
            SwarmIdentity::new(signer.clone(), B256::from([1u8; 32]), spec.clone(), true);
        let identity2 = SwarmIdentity::new(signer, B256::from([2u8; 32]), spec, true);

        // Same signer, different nonce = same eth address, different overlay
        assert_eq!(identity1.ethereum_address(), identity2.ethereum_address());
        assert_ne!(identity1.overlay_address(), identity2.overlay_address());
    }

    #[test]
    fn test_identity_trait_impl() {
        let spec = init_testnet();
        let identity = SwarmIdentity::random(spec, false);

        // Test trait methods
        assert!(!identity.is_full_node());
        // Default welcome message from trait
        assert_eq!(
            identity.welcome_message(),
            Some("Buzzing in from the Rustacean hive")
        );

        let identity = identity.with_welcome_message("Hello, Swarm!");
        assert_eq!(identity.welcome_message(), Some("Hello, Swarm!"));
    }
}
