//! Configuration traits for node infrastructure.
//!
//! This module is protocol-agnostic - it knows nothing about specific network
//! protocols like Swarm. Protocol configuration is handled via the [`NodeProtocolConfig`]
//! trait which protocols implement to provide their specific configuration.

/// Trait for protocol-specific configuration.
///
/// Protocols implement this trait to define their configuration structure.
/// The configuration is combined with generic node infrastructure config
/// via [`vertex_node_core::config::FullNodeConfig<P>`].
pub trait NodeProtocolConfig: Default + Clone {
    /// CLI arguments type for this protocol.
    ///
    /// This should be a clap `Args` struct that can be flattened into a CLI parser.
    type Args: Clone;

    /// Apply CLI argument overrides to this configuration.
    ///
    /// Called after loading config from file/environment to apply
    /// command-line overrides.
    fn apply_args(&mut self, args: &Self::Args);
}
