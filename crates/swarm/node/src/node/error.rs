//! Node construction errors.

use std::fmt;

/// Error during node construction.
#[derive(Debug)]
pub enum NodeBuildError {
    /// Topology behaviour was already taken from infrastructure.
    TopologyBehaviourTaken,
    /// Failed to acquire topology from cell (should never happen in single-threaded context).
    TopologyCellPoisoned,
}

impl fmt::Display for NodeBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TopologyBehaviourTaken => {
                write!(
                    f,
                    "topology behaviour was already taken from infrastructure"
                )
            }
            Self::TopologyCellPoisoned => {
                write!(f, "failed to acquire topology from cell")
            }
        }
    }
}

impl std::error::Error for NodeBuildError {}
