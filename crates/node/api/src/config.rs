//! Configuration traits for node infrastructure.
//!
//! These traits define the configuration parameters for node-level infrastructure
//! such as RPC servers, metrics endpoints, logging, and database storage.
//!
//! This module is protocol-agnostic - it knows nothing about specific network
//! protocols like Swarm. Protocol configuration is handled via the [`NodeProtocolConfig`]
//! trait which protocols implement to provide their specific configuration.
//!
//! # Protocol Configuration
//!
//! The [`NodeProtocolConfig`] trait allows protocols to define their configuration
//! structure. This is used by the generic config loading in `vertex-node-core`
//! to create a combined configuration:
//!
//! ```ignore
//! use vertex_node_core::config::FullNodeConfig;
//! use vertex_swarm_node::ProtocolConfig;
//!
//! // Load combined config (generic infra + Swarm protocol)
//! let config = FullNodeConfig::<ProtocolConfig>::load(path)?;
//! ```

/// Configuration for RPC server (gRPC, REST, etc.).
pub trait NodeRpcConfig {
    /// Whether the gRPC server is enabled.
    fn grpc_enabled(&self) -> bool;

    /// gRPC server listen address.
    fn grpc_addr(&self) -> &str;

    /// gRPC server listen port.
    fn grpc_port(&self) -> u16;
}

/// Configuration for metrics and observability endpoints.
pub trait NodeMetricsConfig {
    /// Whether the metrics HTTP endpoint is enabled.
    fn metrics_enabled(&self) -> bool;

    /// Metrics listen address.
    fn metrics_addr(&self) -> &str;

    /// Metrics listen port.
    fn metrics_port(&self) -> u16;
}

/// Configuration for logging.
///
/// Controls log output format, verbosity, and file rotation.
pub trait NodeLoggingConfig {
    /// Whether logging is enabled.
    fn logging_enabled(&self) -> bool;

    /// Log verbosity level (0 = info, 1 = debug, 2+ = trace).
    fn verbosity(&self) -> u8;

    /// Whether to use JSON format for log output.
    fn json_logging(&self) -> bool;

    /// Optional log filter directive (e.g., "vertex=debug,libp2p=info").
    fn log_filter(&self) -> Option<&str>;

    /// Optional directory for log files.
    fn log_dir(&self) -> Option<&str>;

    /// Maximum log file size in megabytes before rotation.
    fn max_log_file_size_mb(&self) -> u64;

    /// Maximum number of rotated log files to keep.
    fn max_log_files(&self) -> usize;
}

/// Configuration for database storage.
///
/// Controls where persistent data is stored and database-specific settings.
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
    /// Metrics configuration.
    type Metrics: NodeMetricsConfig;
    /// Logging configuration.
    type Logging: NodeLoggingConfig;
    /// Database configuration.
    type Database: NodeDatabaseConfig;

    /// Get RPC server configuration.
    fn rpc(&self) -> &Self::Rpc;

    /// Get metrics configuration.
    fn metrics(&self) -> &Self::Metrics;

    /// Get logging configuration.
    fn logging(&self) -> &Self::Logging;

    /// Get database configuration.
    fn database(&self) -> &Self::Database;
}

/// Trait for protocol-specific configuration.
///
/// Protocols implement this trait to define their configuration structure.
/// The configuration is combined with generic node infrastructure config
/// via [`vertex_node_core::config::FullNodeConfig<P>`].
///
/// # Requirements
///
/// - `Default`: Provides sensible defaults when no config is specified
/// - `Clone`: Config may need to be shared across components
/// - `Serialize + DeserializeOwned`: For config file loading (when `serde` feature enabled)
///
/// # Example
///
/// ```ignore
/// use vertex_node_api::NodeProtocolConfig;
///
/// #[derive(Default, Clone, Serialize, Deserialize)]
/// pub struct ProtocolConfig {
///     pub node_type: SwarmNodeType,
///     pub network: NetworkArgs,
///     // ... other Swarm-specific fields
/// }
///
/// impl NodeProtocolConfig for ProtocolConfig {
///     type Args = ProtocolArgs;
///
///     fn apply_args(&mut self, args: &Self::Args) {
///         self.node_type = args.node_type;
///         self.network = args.network.clone();
///         // ... apply other overrides
///     }
/// }
/// ```
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
