//! Local node identity for Swarm networks.
//!
//! Provides [`Identity`], the standard implementation of the [`SwarmIdentity`] trait.
//! Overlay address derivation: `keccak256(eth_address || network_id || nonce)`

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub mod args;
pub mod keystore;

use alloy_primitives::{Address, B256, ChainId, Signature};
use alloy_signer::SignerSync;
use alloy_signer::k256::ecdsa::SigningKey;
use alloy_signer_local::LocalSigner;
use nectar_primitives::SwarmAddress;
use std::sync::Arc;
use vertex_swarm_api::{SwarmIdentity, SwarmIdentityConfig, SwarmNodeType};
use vertex_swarm_primitives::{NetworkId, Nonce, OverlaySigner, compute_overlay};
use vertex_swarm_spec::{HasSpec, Loggable, Spec, SwarmSpec};

pub use args::IdentityArgs;
pub use keystore::{create_and_save_signer, load_signer_from_keystore, resolve_password};
pub use vertex_swarm_api::IdentityError;
pub use vertex_swarm_api::SwarmIdentity as IdentityTrait;

/// Samples a cryptographically random [`Nonce`] from the runtime CSPRNG facade.
///
/// A nonce is 32 random bytes with no key-derivation step, so filling it from
/// the wasm-safe facade keeps the same entropy guarantee as the upstream
/// `Nonce::random()` while avoiding its thread-local RNG.
pub(crate) fn random_nonce() -> Nonce {
    let mut bytes = [0u8; 32];
    vertex_util_runtime::rand::fill_bytes(&mut bytes);
    Nonce::new(bytes)
}

/// Local node identity containing signing key, nonce, and network spec.
///
/// Holds an `Arc<Spec>` for shared access to the network specification.
/// Wrap in `Arc<Identity>` for sharing across components.
///
/// `Clone` is cheap: all heap state lives behind `Arc`, so cloning bumps
/// reference counts only. This enables the integration harness (see
/// `vertex-swarm-test-utils::cluster`) to construct a single persistent
/// `Identity` and hand owned copies to each node builder, mirroring how a
/// real bootnode reuses a stable keystore across restarts.
#[derive(Clone)]
pub struct Identity {
    spec: Arc<Spec>,
    signer: Arc<LocalSigner<SigningKey>>,
    nonce: Nonce,
    /// Cached at construction time.
    overlay: SwarmAddress,
    node_type: SwarmNodeType,
    welcome_message: Option<String>,
    /// True if this identity was created from a random ephemeral signer rather
    /// than loaded/saved through a keystore. Bootnodes must never run with
    /// `ephemeral == true` because their overlay address is a network
    /// contract.
    ephemeral: bool,
}

impl Identity {
    /// Creates a new identity from a signer, nonce, spec, and node type.
    ///
    /// Identities created via `new` are considered persistent — callers are
    /// expected to have sourced the signer from a keystore.
    pub fn new(
        signer: LocalSigner<SigningKey>,
        nonce: Nonce,
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
            ephemeral: false,
        }
    }

    /// Creates a random ephemeral identity for testing.
    pub fn random(spec: Arc<Spec>, node_type: SwarmNodeType) -> Self {
        // Avoid the upstream `*::random()` helpers, which seed from a
        // thread-local RNG. The nonce is filled from the wasm-safe runtime
        // facade. The signer keygen runs through alloy, which pins rand 0.8,
        // so it takes a rand 0.8 OS RNG (getrandom-backed, no thread-local)
        // rather than the rand 0.9 facade; the pin tracks the alloy dependency,
        // not a wasm gap.
        let nonce = random_nonce();
        let signer = LocalSigner::random_with(&mut rand_08::rngs::OsRng);
        let overlay = compute_overlay(&signer.address(), spec.network_id(), &nonce);
        Self {
            spec,
            signer: Arc::new(signer),
            nonce,
            overlay,
            node_type,
            welcome_message: None,
            ephemeral: true,
        }
    }

    /// Creates a random ephemeral identity whose overlay's leading `prefix_bits`
    /// equal `prefix_value` (taken from the high bits of `prefix_value`).
    ///
    /// The signer is random; only the nonce is ground, so the overlay
    /// (`keccak256(address || network_id || nonce)`) lands in the target slice
    /// of the address space. A sharded browser download gives worker `k` the
    /// prefix `k` over `log2(K)` bits, so its Kademlia neighbourhood covers the
    /// peers closest to the chunks it is assigned, collapsing the
    /// not-connected retrieval tax. `prefix_bits` is clamped to 8 (one byte);
    /// grinding cost is `2^prefix_bits` keccaks on average.
    pub fn random_in_prefix(
        spec: Arc<Spec>,
        node_type: SwarmNodeType,
        prefix_bits: u8,
        prefix_value: u8,
    ) -> Self {
        let bits = prefix_bits.min(8);
        let signer = LocalSigner::random_with(&mut rand_08::rngs::OsRng);
        let address = signer.address();
        let network_id = spec.network_id();
        // Mask the top `bits` of a byte; the target is `prefix_value`'s top
        // `bits` bits, matched against the overlay's first byte.
        let mask: u8 = if bits == 0 { 0 } else { 0xFFu8 << (8 - bits) };
        let target = prefix_value & mask;
        let mut bytes = [0u8; 32];
        let nonce = loop {
            vertex_util_runtime::rand::fill_bytes(&mut bytes);
            let candidate = Nonce::new(bytes);
            let overlay = compute_overlay(&address, network_id, &candidate);
            let top = overlay.as_bytes().first().copied().unwrap_or(0);
            if top & mask == target {
                break candidate;
            }
        };
        let overlay = compute_overlay(&address, network_id, &nonce);
        Self {
            spec,
            signer: Arc::new(signer),
            nonce,
            overlay,
            node_type,
            welcome_message: None,
            ephemeral: true,
        }
    }

    /// Sets a custom welcome message.
    pub fn with_welcome_message(mut self, message: impl Into<String>) -> Self {
        self.welcome_message = Some(message.into());
        self
    }
}

impl SwarmIdentityConfig for Identity {
    fn ephemeral(&self) -> bool {
        self.ephemeral
    }
}

impl HasSpec for Identity {
    fn spec(&self) -> &Arc<Spec> {
        &self.spec
    }
}

impl SignerSync for Identity {
    fn sign_hash_sync(&self, hash: &B256) -> alloy_signer::Result<Signature> {
        self.signer.sign_hash_sync(hash)
    }

    fn chain_id_sync(&self) -> Option<ChainId> {
        self.signer.chain_id_sync()
    }
}

impl OverlaySigner for Identity {
    fn address(&self) -> Address {
        self.signer.address()
    }

    fn network_id(&self) -> NetworkId {
        self.spec.network_id()
    }

    fn nonce(&self) -> Nonce {
        self.nonce
    }

    /// Returns the overlay cached at construction rather than recomputing it.
    fn overlay(&self) -> SwarmAddress {
        self.overlay
    }
}

impl SwarmIdentity for Identity {
    type Spec = Arc<Spec>;
    type Signer = LocalSigner<SigningKey>;

    fn spec(&self) -> &Self::Spec {
        &self.spec
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
        assert!(identity.is_storer());
    }

    #[test]
    fn same_signer_and_nonce_same_overlay() {
        let spec = init_testnet();
        let signer = LocalSigner::random();
        let nonce = Nonce::new([0x42u8; 32]);

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
            Nonce::new([1u8; 32]),
            spec.clone(),
            SwarmNodeType::Storer,
        );
        let id2 = Identity::new(signer, Nonce::new([2u8; 32]), spec, SwarmNodeType::Storer);

        assert_eq!(id1.ethereum_address(), id2.ethereum_address());
        assert_ne!(id1.overlay_address(), id2.overlay_address());
    }

    #[test]
    fn random_in_prefix_lands_in_slice() {
        let spec = init_testnet();
        // 2-bit prefix: worker indices 0..4 map to top-byte prefixes
        // 0x00, 0x40, 0x80, 0xC0.
        for k in 0u8..4 {
            let value = k << 6; // top 2 bits = k
            let id = Identity::random_in_prefix(spec.clone(), SwarmNodeType::Client, 2, value);
            let top = id.overlay_address().as_bytes().first().copied().unwrap();
            assert_eq!(top >> 6, k, "overlay top 2 bits must equal worker index");
        }
    }

    #[test]
    fn random_in_prefix_zero_bits_is_unconstrained() {
        let spec = init_testnet();
        // Zero prefix bits imposes no constraint and still yields a valid overlay.
        let id = Identity::random_in_prefix(spec, SwarmNodeType::Client, 0, 0xFF);
        assert!(!id.overlay_address().is_zero());
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
