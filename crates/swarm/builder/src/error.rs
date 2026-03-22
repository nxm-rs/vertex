//! Error types for Swarm node building.

use thiserror::Error;

use crate::config::SwarmConfigError;

/// Error type for Swarm node building and launching.
#[derive(Debug, Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum SwarmNodeError {
    /// Configuration validation error.
    #[error("config error: {0}")]
    Config(#[from] SwarmConfigError),

    /// Build error from protocol.
    #[error("build error: {0}")]
    Build(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Launch error.
    #[error("launch error: {0}")]
    Launch(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Node type not implemented.
    #[error("node type not implemented: {0}")]
    NotImplemented(String),
}
