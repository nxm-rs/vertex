//! Error types for Swarm node building.

use thiserror::Error;

/// Error type for Swarm node building and launching.
#[derive(Debug, Error)]
pub enum SwarmNodeError {
    /// Build error from protocol.
    #[error("build error: {0}")]
    Build(String),

    /// Launch error.
    #[error("launch error: {0}")]
    Launch(String),

    /// Node type not implemented.
    #[error("node type not implemented: {0}")]
    NotImplemented(String),

    /// Runtime error.
    #[error("runtime error: {0}")]
    Runtime(#[from] eyre::Report),
}
