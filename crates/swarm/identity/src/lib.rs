//! Local node identity for Swarm networks.
//!
//! Provides [`Identity`], the standard implementation of the [`SwarmIdentity`] trait.
//! Overlay address derivation: `keccak256(eth_address || network_id || nonce)`

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

use alloy_primitives::B256;
use alloy_signer::k256::ecdsa::SigningKey;
use alloy_signer_local::LocalSigner;
use nectar_primitives::SwarmAddress;
use std::sync::Arc;
use vertex_swarm_api::{SwarmIdentity, SwarmNodeType};
use vertex_swarm_primitives::compute_overlay;
use vertex_swarmspec::{Hive, Loggable, SwarmSpec};

pub use vertex_swarm_api::SwarmIdentity as IdentityTrait;

/// Local node identity containing signing key, nonce, and network spec.
///
/// Caches the overlay address at construction time.
#[derive(Clone)]
pub struct Identity {
    /// Network specification (network_id, bootnodes, etc.)
    spec: Arc<Hive>,
    /// Signing key for this node.
    signer: Arc<LocalSigner<SigningKey>>,
    /// Nonce for overlay address derivation.
    nonce: B256,
    /// Cached overlay address.
    overlay: SwarmAddress,
    /// Node capability level.
    node_type: SwarmNodeType,
    /// Custom welcome message for peers.
    welcome_message: Option<String>,
}

impl Identity {
    /// Creates a new identity from a signer, nonce, spec, and node type.
    pub fn new(
        signer: LocalSigner<SigningKey>,
        nonce: B256,
        spec: Arc<Hive>,
        node_type: SwarmNodeType,
    ) -> Self {
        let overlay = compute_overlay(&signer.address(), spec.network_id(), &nonce);
        Self {
            spec,
            signer: Arc::new(signer),
            nonce,
            overlay,
            node_type,
            welcome_message: None,
        }
    }

    /// Creates a random ephemeral identity for testing.
    pub fn random(spec: Arc<Hive>, node_type: SwarmNodeType) -> Self {
        use rand::Rng;
        let mut rng = rand::rng();

        let mut key_bytes = [0u8; 32];
        rng.fill(&mut key_bytes);
        let signing_key =
            SigningKey::from_slice(&key_bytes).expect("32 bytes is valid for secp256k1");
        let signer = LocalSigner::from_signing_key(signing_key);

        let mut nonce_bytes = [0u8; 32];
        rng.fill(&mut nonce_bytes);
        let nonce = B256::from(nonce_bytes);

        Self::new(signer, nonce, spec, node_type)
    }

    /// Sets a custom welcome message.
    pub fn with_welcome_message(mut self, message: impl Into<String>) -> Self {
        self.welcome_message = Some(message.into());
        self
    }
}

impl SwarmIdentity for Identity {
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

    fn node_type(&self) -> SwarmNodeType {
        self.node_type
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

impl Loggable for Identity {
    fn log(&self) {
        use tracing::{debug, info};
        info!("Identity:");
        info!("  Ethereum address: {}", self.signer.address());
        debug!("  Nonce: {}", self.nonce);
        info!("  Overlay address: {}", self.overlay_address());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarmspec::init_testnet;

    #[test]
    fn random_identity() {
        let spec = init_testnet();
        let identity = Identity::random(spec, SwarmNodeType::Storer);

        assert!(!identity.ethereum_address().is_zero());
        assert!(!identity.overlay_address().is_zero());
        assert!(identity.is_full_node());
    }

    #[test]
    fn same_signer_and_nonce_same_overlay() {
        let spec = init_testnet();

        let mut rng = rand::rng();
        let mut key_bytes = [0u8; 32];
        rand::Rng::fill(&mut rng, &mut key_bytes);
        let signing_key = SigningKey::from_slice(&key_bytes).unwrap();
        let signer = LocalSigner::from_signing_key(signing_key);
        let nonce = B256::from([0x42u8; 32]);

        let id1 = Identity::new(signer.clone(), nonce, spec.clone(), SwarmNodeType::Storer);
        let id2 = Identity::new(signer, nonce, spec, SwarmNodeType::Storer);

        assert_eq!(id1.overlay_address(), id2.overlay_address());
        assert_eq!(id1.ethereum_address(), id2.ethereum_address());
    }

    #[test]
    fn different_nonce_different_overlay() {
        let spec = init_testnet();

        let mut rng = rand::rng();
        let mut key_bytes = [0u8; 32];
        rand::Rng::fill(&mut rng, &mut key_bytes);
        let signing_key = SigningKey::from_slice(&key_bytes).unwrap();
        let signer = LocalSigner::from_signing_key(signing_key);

        let id1 = Identity::new(
            signer.clone(),
            B256::from([1u8; 32]),
            spec.clone(),
            SwarmNodeType::Storer,
        );
        let id2 = Identity::new(signer, B256::from([2u8; 32]), spec, SwarmNodeType::Storer);

        assert_eq!(id1.ethereum_address(), id2.ethereum_address());
        assert_ne!(id1.overlay_address(), id2.overlay_address());
    }

    #[test]
    fn welcome_message() {
        let spec = init_testnet();
        let identity = Identity::random(spec, SwarmNodeType::Client);

        assert_eq!(
            identity.welcome_message(),
            Some("Buzzing in from the Rustacean hive")
        );

        let identity = identity.with_welcome_message("Hello!");
        assert_eq!(identity.welcome_message(), Some("Hello!"));
    }
}
