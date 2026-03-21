#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
//! Test utilities and mocks for vertex-swarm crates.
//!
//! This crate provides shared test infrastructure to reduce duplication
//! across the vertex codebase. It consolidates common test patterns:
//!
//! - [`MockIdentity`] - A configurable mock implementation of `SwarmIdentity`
//! - [`MockTopology`] - A configurable mock implementation of `SwarmTopology`
//! - Helper functions for creating deterministic test fixtures
//!
//! # Usage
//!
//! Add to your crate's `[dev-dependencies]`:
//!
//! ```toml
//! [dev-dependencies]
//! vertex-swarm-test-utils.workspace = true
//! ```
//!
//! Then import what you need:
//!
//! ```ignore
//! use vertex_swarm_test_utils::{test_identity, test_peer_id, MockIdentity};
//! ```

pub mod identity;
pub mod peer;
pub mod spec;
pub mod topology;

// Re-exports for convenience
pub use identity::{MockIdentity, test_identity, test_identity_arc, test_identity_with_type};
pub use peer::{
    make_overlay, make_swarm_peer_minimal, test_overlay, test_peer, test_peer_id, test_swarm_peer,
};
pub use spec::{TEST_NETWORK_ID, test_spec, test_spec_isolated, test_spec_with_network_id};
pub use topology::MockTopology;

// Re-export commonly used types for convenience
pub use vertex_swarm_identity::Identity;
