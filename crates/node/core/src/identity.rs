//! Node identity management.
//!
//! This module handles the node's cryptographic identity, including:
//! - Generating or loading the signing key
//! - Creating the overlay address from the signing key and nonce
//! - Providing the HandshakeConfig implementation for the handshake protocol

use alloy_primitives::{Address, B256};
use alloy_signer::k256::ecdsa::SigningKey;
use alloy_signer_local::LocalSigner;
use eyre::Result;
use nectar_primitives::SwarmAddress;
use rand::RngCore;
use std::sync::Arc;
use vertex_net_handshake::HandshakeConfig;
use vertex_net_primitives_traits::calculate_overlay_address;

/// Node identity containing signing credentials and overlay address.
///
/// This is the core cryptographic identity of the node, used for:
/// - Signing handshake messages
/// - Deriving the overlay address
/// - Participating in the Swarm network
#[derive(Clone)]
pub struct NodeIdentity {
    /// The network ID (1 for mainnet, 10 for testnet, etc.)
    network_id: u64,

    /// The signing key for this node.
    signer: Arc<LocalSigner<SigningKey>>,

    /// The nonce used for overlay address derivation.
    nonce: B256,

    /// Whether this node operates as a full node.
    is_full_node: bool,

    /// Optional welcome message for peers.
    welcome_message: Option<String>,
}

impl std::fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeIdentity")
            .field("network_id", &self.network_id)
            .field("nonce", &self.nonce)
            .field("is_full_node", &self.is_full_node)
            .field("welcome_message", &self.welcome_message)
            .finish_non_exhaustive()
    }
}

impl NodeIdentity {
    /// Create a new random node identity.
    ///
    /// This generates a new random signing key and nonce. Use this for
    /// ephemeral nodes or initial setup.
    pub fn random(network_id: u64, is_full_node: bool) -> Result<Self> {
        let mut rng = rand::thread_rng();

        // Generate random signing key
        let mut key_bytes = [0u8; 32];
        rng.fill_bytes(&mut key_bytes);
        let signing_key = SigningKey::from_slice(&key_bytes)?;
        let signer = LocalSigner::from_signing_key(signing_key);

        // Generate random nonce
        let mut nonce_bytes = [0u8; 32];
        rng.fill_bytes(&mut nonce_bytes);
        let nonce = B256::from(nonce_bytes);

        Ok(Self {
            network_id,
            signer: Arc::new(signer),
            nonce,
            is_full_node,
            welcome_message: None,
        })
    }

    /// Create a node identity from an existing signing key and nonce.
    pub fn from_key_and_nonce(
        network_id: u64,
        signing_key: SigningKey,
        nonce: B256,
        is_full_node: bool,
    ) -> Self {
        let signer = LocalSigner::from_signing_key(signing_key);
        Self {
            network_id,
            signer: Arc::new(signer),
            nonce,
            is_full_node,
            welcome_message: None,
        }
    }

    /// Get the network ID.
    pub fn network_id(&self) -> u64 {
        self.network_id
    }

    /// Set the welcome message.
    pub fn with_welcome_message(mut self, message: impl Into<String>) -> Self {
        self.welcome_message = Some(message.into());
        self
    }

    /// Get the signer.
    pub fn signer(&self) -> &Arc<LocalSigner<SigningKey>> {
        &self.signer
    }

    /// Get the nonce.
    pub fn nonce(&self) -> B256 {
        self.nonce
    }

    /// Check if this is a full node.
    pub fn is_full_node(&self) -> bool {
        self.is_full_node
    }

    /// Get the Ethereum address derived from the signing key.
    pub fn ethereum_address(&self) -> Address {
        self.signer.address()
    }

    /// Get the overlay address derived from the Ethereum address, network ID, and nonce.
    ///
    /// The overlay address is: Keccak256(ethereum_address || network_id || nonce)
    pub fn overlay_address(&self) -> SwarmAddress {
        calculate_overlay_address(&self.ethereum_address(), self.network_id, &self.nonce)
    }
}

impl HandshakeConfig for NodeIdentity {
    fn network_id(&self) -> u64 {
        self.network_id
    }

    fn nonce(&self) -> B256 {
        self.nonce
    }

    fn signer(&self) -> Arc<LocalSigner<SigningKey>> {
        self.signer.clone()
    }

    fn is_full_node(&self) -> bool {
        self.is_full_node
    }

    fn welcome_message(&self) -> Option<String> {
        self.welcome_message.clone()
    }
}
