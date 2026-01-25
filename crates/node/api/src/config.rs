//! Configuration traits for node infrastructure.
//!
//! These traits define the configuration parameters for node-level infrastructure
//! such as RPC servers, metrics endpoints, logging, and database storage.

/// Configuration for RPC server (gRPC, REST, etc.).
pub trait RpcConfig {
    /// Whether the gRPC server is enabled.
    fn grpc_enabled(&self) -> bool;

    /// gRPC server listen address.
    fn grpc_addr(&self) -> &str;

    /// gRPC server listen port.
    fn grpc_port(&self) -> u16;
}

/// Configuration for metrics and observability endpoints.
pub trait MetricsConfig {
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
pub trait LoggingConfig {
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
pub trait DatabaseConfig {
    /// Root data directory for all node data.
    fn data_dir(&self) -> Option<&str>;

    /// Whether to use an in-memory database (no persistence).
    fn memory_only(&self) -> bool;

    /// Database cache size in megabytes.
    fn cache_size_mb(&self) -> Option<u64>;
}

/// Combined node configuration.
///
/// This trait provides access to all node-level configuration sections,
/// combining both Swarm protocol config and infrastructure config.
pub trait NodeConfig {
    /// Swarm protocol configuration.
    type Swarm: vertex_swarm_api::SwarmConfig;
    /// RPC server configuration.
    type Rpc: RpcConfig;
    /// Metrics configuration.
    type Metrics: MetricsConfig;
    /// Logging configuration.
    type Logging: LoggingConfig;
    /// Database configuration.
    type Database: DatabaseConfig;

    /// Get Swarm protocol configuration.
    fn swarm(&self) -> &Self::Swarm;

    /// Get RPC server configuration.
    fn rpc(&self) -> &Self::Rpc;

    /// Get metrics configuration.
    fn metrics(&self) -> &Self::Metrics;

    /// Get logging configuration.
    fn logging(&self) -> &Self::Logging;

    /// Get database configuration.
    fn database(&self) -> &Self::Database;
}
