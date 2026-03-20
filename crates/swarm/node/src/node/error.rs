//! Node construction errors.

/// Error during node construction.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum NodeBuildError {
    /// Topology behaviour was already taken from infrastructure.
    #[error("topology behaviour was already taken from infrastructure")]
    TopologyBehaviourTaken,
    /// Failed to acquire topology from cell (should never happen in single-threaded context).
    #[error("failed to acquire topology from cell")]
    TopologyCellPoisoned,
}
