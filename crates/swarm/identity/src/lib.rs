//! Local node identity for Swarm networks.
//!
//! Provides [`Identity`], the standard implementation of the [`SwarmIdentity`] trait.
//! Overlay address derivation: `keccak256(eth_address || network_id || nonce)`

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub mod args;
pub mod keystore;

use alloy_primitives::B256;
use alloy_signer::k256::ecdsa::SigningKey;
use alloy_signer_local::LocalSigner;
use nectar_primitives::SwarmAddress;
use std::sync::Arc;
use vertex_swarm_api::{SwarmIdentity, SwarmNodeType};
use vertex_swarm_primitives::compute_overlay;
use vertex_swarm_spec::{HasSpec, Loggable, Spec, SwarmSpec};

pub use args::IdentityArgs;
pub use keystore::{create_and_save_signer, load_signer_from_keystore, resolve_password};
pub use vertex_swarm_api::SwarmIdentity as IdentityTrait;

/// Local node identity containing signing key, nonce, and network spec.
///
/// Holds an `Arc<Spec>` for shared access to the network specification.
/// Wrap in `Arc<Identity>` for sharing across components.
pub struct Identity {
    spec: Arc<Spec>,
    signer: Arc<LocalSigner<SigningKey>>,
    nonce: B256,
    /// Cached at construction time.
    overlay: SwarmAddress,
    node_type: SwarmNodeType,
    welcome_message: Option<String>,
}

impl Identity {
    /// Creates a new identity from a signer, nonce, spec, and node type.
    pub fn new(
        signer: LocalSigner<SigningKey>,
        nonce: B256,
        spec: Arc<Spec>,
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
    pub fn random(spec: Arc<Spec>, node_type: SwarmNodeType) -> Self {
        let nonce = B256::from(rand::random::<[u8; 32]>());
        Self::new(LocalSigner::random(), nonce, spec, node_type)
    }

    /// Sets a custom welcome message.
    pub fn with_welcome_message(mut self, message: impl Into<String>) -> Self {
        self.welcome_message = Some(message.into());
        self
    }
}

impl HasSpec for Identity {
    fn spec(&self) -> &Arc<Spec> {
        &self.spec
    }
}

impl SwarmIdentity for Identity {
    type Spec = Arc<Spec>;
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
    use vertex_swarm_spec::init_testnet;

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
        let signer = LocalSigner::random();
        let nonce = B256::from([0x42u8; 32]);

        let id1 = Identity::new(signer.clone(), nonce, spec.clone(), SwarmNodeType::Storer);
        let id2 = Identity::new(signer, nonce, spec, SwarmNodeType::Storer);

        assert_eq!(id1.overlay_address(), id2.overlay_address());
        assert_eq!(id1.ethereum_address(), id2.ethereum_address());
    }

    #[test]
    fn different_nonce_different_overlay() {
        let spec = init_testnet();
        let signer = LocalSigner::random();

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

    #[test]
    fn has_spec_trait() {
        let spec = init_testnet();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Client);

        // HasSpec returns &Arc<Spec>
        let spec_ref: &Arc<Spec> = HasSpec::spec(&identity);
        assert_eq!(spec_ref.network_id(), spec.network_id());

        // Can clone the Arc without taking ownership
        let cloned: Arc<Spec> = spec_ref.clone();
        assert_eq!(cloned.network_id(), spec.network_id());
    }
}
