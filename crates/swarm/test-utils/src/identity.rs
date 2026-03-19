//! Mock identity implementations and test helpers.

use alloy_primitives::B256;
use alloy_signer_local::LocalSigner;
use nectar_primitives::SwarmAddress;
use std::sync::Arc;
use vertex_swarm_api::{SwarmIdentity, SwarmNodeType};
use vertex_swarm_identity::Identity;
use vertex_swarm_spec::Spec;

/// A mock identity for testing Swarm components.
///
/// Use this when you need control over the overlay address, or when
/// the real `Identity` type adds unwanted complexity to tests.
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_test_utils::MockIdentity;
///
/// // Create with specific overlay
/// let mock = MockIdentity::with_first_byte(0x42);
///
/// // Or with builder pattern
/// let mock = MockIdentity::with_first_byte(0x00)
///     .with_node_type(SwarmNodeType::Client)
///     .with_nonce(B256::repeat_byte(0xff));
/// ```
#[derive(Clone)]
pub struct MockIdentity {
    overlay: SwarmAddress,
    signer: Arc<LocalSigner<alloy_signer::k256::ecdsa::SigningKey>>,
    spec: Arc<Spec>,
    node_type: SwarmNodeType,
    nonce: B256,
}

impl std::fmt::Debug for MockIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockIdentity")
            .field("overlay", &self.overlay)
            .field("node_type", &self.node_type)
            .finish_non_exhaustive()
    }
}

impl MockIdentity {
    /// Create a mock identity with the given overlay address.
    pub fn with_overlay(overlay: SwarmAddress) -> Self {
        let signer = LocalSigner::random();
        Self {
            overlay,
            signer: Arc::new(signer),
            spec: vertex_swarm_spec::init_testnet(),
            node_type: SwarmNodeType::Storer,
            nonce: B256::ZERO,
        }
    }

    /// Create a mock identity with a specific first byte for the overlay.
    ///
    /// Useful for testing Kademlia distance calculations where you need
    /// to control XOR distances between peers.
    pub fn with_first_byte(byte: u8) -> Self {
        Self::with_overlay(SwarmAddress::with_first_byte(byte))
    }

    /// Set the node type for this mock identity.
    #[must_use]
    pub fn with_node_type(mut self, node_type: SwarmNodeType) -> Self {
        self.node_type = node_type;
        self
    }

    /// Set the nonce for this mock identity.
    #[must_use]
    pub fn with_nonce(mut self, nonce: B256) -> Self {
        self.nonce = nonce;
        self
    }

    /// Set a custom spec for this mock identity.
    #[must_use]
    pub fn with_spec(mut self, spec: Arc<Spec>) -> Self {
        self.spec = spec;
        self
    }
}

impl SwarmIdentity for MockIdentity {
    type Spec = Spec;
    type Signer = LocalSigner<alloy_signer::k256::ecdsa::SigningKey>;

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

    fn overlay_address(&self) -> SwarmAddress {
        self.overlay
    }
}

/// Create a random test identity with testnet spec.
///
/// This is the most common test identity pattern across the codebase.
/// The identity has a random overlay address and uses `SwarmNodeType::Client`.
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_test_utils::test_identity;
///
/// let identity = test_identity();
/// assert_eq!(identity.node_type(), SwarmNodeType::Client);
/// ```
pub fn test_identity() -> Identity {
    Identity::random(vertex_swarm_spec::init_testnet(), SwarmNodeType::Client)
}

/// Create a random test identity wrapped in Arc.
///
/// Use when components require `Arc<Identity>`.
pub fn test_identity_arc() -> Arc<Identity> {
    Arc::new(test_identity())
}

/// Create a test identity with a specific node type.
pub fn test_identity_with_type(node_type: SwarmNodeType) -> Identity {
    Identity::random(vertex_swarm_spec::init_testnet(), node_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_identity_with_overlay() {
        let overlay = SwarmAddress::with_first_byte(0x42);
        let mock = MockIdentity::with_overlay(overlay);

        assert_eq!(mock.overlay_address(), overlay);
        assert_eq!(mock.node_type(), SwarmNodeType::Storer);
        assert_eq!(mock.nonce(), B256::ZERO);
    }

    #[test]
    fn test_mock_identity_builder() {
        let mock = MockIdentity::with_first_byte(0x00)
            .with_node_type(SwarmNodeType::Client)
            .with_nonce(B256::repeat_byte(0xff));

        assert_eq!(mock.node_type(), SwarmNodeType::Client);
        assert_eq!(mock.nonce(), B256::repeat_byte(0xff));
    }

    #[test]
    fn test_identity_helpers() {
        let id1 = test_identity();
        let id2 = test_identity();

        // Each call creates a new random identity
        assert_ne!(id1.overlay_address(), id2.overlay_address());
        assert_eq!(id1.node_type(), SwarmNodeType::Client);
    }

    #[test]
    fn test_identity_with_node_type() {
        let storer = test_identity_with_type(SwarmNodeType::Storer);
        assert_eq!(storer.node_type(), SwarmNodeType::Storer);
    }
}
