//! Configuration traits for node infrastructure.
//!
//! These traits define the configuration parameters for node-level infrastructure
//! such as RPC servers and database storage.
//!
//! This module is protocol-agnostic - it knows nothing about specific network
//! protocols like Swarm. Protocol configuration is handled via the [`NodeProtocolConfig`]
//! trait which protocols implement to provide their specific configuration.

/// Configuration for RPC server (gRPC, REST, etc.).
pub trait NodeRpcConfig {
    /// Whether the gRPC server is enabled.
    fn grpc_enabled(&self) -> bool;

    /// gRPC server listen address.
    fn grpc_addr(&self) -> &str;

    /// gRPC server listen port.
    fn grpc_port(&self) -> u16;
}

/// Configuration for database storage.
pub trait NodeDatabaseConfig {
    /// Root data directory for all node data.
    fn data_dir(&self) -> Option<&str>;

    /// Whether to use an in-memory database (no persistence).
    fn memory_only(&self) -> bool;

    /// Database cache size in megabytes.
    fn cache_size_mb(&self) -> Option<u64>;
}

/// Combined infrastructure configuration.
///
/// This trait provides access to all node-level infrastructure configuration.
/// It is protocol-agnostic - protocol-specific configuration is handled
/// separately by the protocol layer.
pub trait NodeConfig {
    /// RPC server configuration.
    type Rpc: NodeRpcConfig;
    /// Database configuration.
    type Database: NodeDatabaseConfig;

    /// Get RPC server configuration.
    fn rpc(&self) -> &Self::Rpc;

    /// Get database configuration.
    fn database(&self) -> &Self::Database;
}

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
