//! Test helpers for network specifications.

use std::sync::Arc;
use vertex_swarm_spec::{Spec, SpecBuilder};

/// Create a testnet spec with default settings.
///
/// This is the most common spec used in tests. Returns a cached
/// Arc to the testnet spec.
pub fn test_spec() -> Arc<Spec> {
    vertex_swarm_spec::init_testnet()
}

/// Create a testnet spec with a custom network ID.
///
/// Useful for tests that need isolated networks that won't
/// accidentally connect to real testnets.
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_test_utils::test_spec_with_network_id;
///
/// let spec = test_spec_with_network_id(999999);
/// assert_eq!(spec.network_id(), 999999);
/// ```
pub fn test_spec_with_network_id(network_id: u64) -> Arc<Spec> {
    Arc::new(SpecBuilder::testnet().network_id(network_id).build())
}

/// Default test network ID for isolated test networks.
///
/// This value (1234567890) is used consistently across tests that need
/// an isolated network that won't conflict with real networks.
pub const TEST_NETWORK_ID: u64 = 1234567890;

/// Create a testnet spec with the standard test network ID.
///
/// Returns a spec with `network_id = 1234567890`, ensuring tests
/// are isolated from real networks.
pub fn test_spec_isolated() -> Arc<Spec> {
    test_spec_with_network_id(TEST_NETWORK_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_api::SwarmSpec;

    #[test]
    fn test_spec_is_testnet() {
        let spec = test_spec();
        // The spec should be a testnet configuration
        assert!(spec.network_id() > 0);
    }

    #[test]
    fn test_spec_isolated_uses_constant() {
        let spec = test_spec_isolated();
        assert_eq!(spec.network_id(), TEST_NETWORK_ID);
    }

    #[test]
    fn test_custom_network_id() {
        let spec = test_spec_with_network_id(42);
        assert_eq!(spec.network_id(), 42);
    }
}
