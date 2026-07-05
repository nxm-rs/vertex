#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
//! Test utilities and mocks for vertex-swarm crates.
//!
//! Shared test infrastructure so suites do not re-roll fixtures:
//!
//! - [`MockIdentity`], [`MockTopology`], [`MockStorage`], [`MockReserve`]:
//!   configurable mocks of the core node traits
//! - `peer`, `spec`, `vectors`: deterministic fixtures and wire vectors
//! - `strategies` (feature `proptest`): proptest strategies for wire types
//! - `harness` (feature `harness`): seeded behaviour-level swarm harness
//! - `cluster` (features `cluster`/`cluster-storer`): in-process multi-node
//!   cluster rig
//!
//! Consume it from `[dev-dependencies]` only.

#[cfg(feature = "cluster")]
pub mod cluster;
#[cfg(feature = "harness")]
pub mod harness;
pub mod identity;
pub mod peer;
pub mod spec;
pub mod storage;
#[cfg(feature = "proptest")]
pub mod strategies;
pub mod topology;
pub mod vectors;

// Re-exports for convenience
pub use identity::{MockIdentity, test_identity, test_identity_arc, test_identity_with_type};
pub use peer::{
    make_overlay, make_swarm_peer_minimal, test_keypair, test_overlay, test_peer, test_peer_id,
    test_signed_swarm_peer, test_swarm_peer, test_swarm_peer_with_timestamp,
};
pub use spec::{TEST_NETWORK_ID, test_spec, test_spec_isolated, test_spec_with_network_id};
pub use storage::{MockReserve, MockStorage};
pub use topology::MockTopology;
pub use vectors::{
    Vector, assert_bytes_eq, assert_bytes_eq_hex, check_each, hex_array, hex_vec, push_uvarint,
    uvarint,
};

// Re-export commonly used types for convenience
pub use vertex_swarm_identity::Identity;
