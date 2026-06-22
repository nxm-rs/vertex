//! Error types for Swarm node building.

use thiserror::Error;

use vertex_swarm_api::SwarmNodeType;

/// Error type for Swarm node building and launching.
#[derive(Debug, Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum SwarmNodeError {
    /// Build error from protocol.
    #[error("build error: {0}")]
    Build(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Launch error.
    #[error("launch error: {0}")]
    Launch(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Node type not implemented.
    #[error("node type not implemented: {0}")]
    NotImplemented(String),

    /// Chain service construction or validation failed.
    #[cfg(feature = "chain")]
    #[error("chain error: {0}")]
    Chain(String),

    /// A chain-needing node type (a storer, a SWAP-enabled client) could not
    /// resolve a chain and may not degrade chainless.
    #[error(
        "node type {node_type} requires an Ethereum chain connection, but none could be resolved: \
         set --chain.rpc-url, use a network with a canonical contract deployment, and build with \
         the `chain` feature"
    )]
    ChainRequired {
        /// The node type that hard-failed for want of a chain.
        node_type: SwarmNodeType,
    },
}
