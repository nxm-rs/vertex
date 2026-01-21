//! Command-line interface for the Vertex Swarm node.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// Vertex Swarm - Ethereum Swarm Node Implementation
#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Logging configuration
    #[command(flatten)]
    pub logs: LogArgs,

    /// Subcommand to execute
    #[command(subcommand)]
    pub command: Commands,
}

/// Swarm node commands
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run a Swarm node
    Node(NodeArgs),
}

/// Arguments for the 'node' command
#[derive(Debug, Args)]
pub struct NodeArgs {
    /// Data directory configuration
    #[command(flatten)]
    pub datadir: DataDirArgs,

    /// Network configuration
    #[command(flatten)]
    pub network: NetworkArgs,

    /// Storage configuration
    #[command(flatten)]
    pub storage: StorageArgs,

    /// API configuration
    #[command(flatten)]
    pub api: ApiArgs,

    /// Identity configuration
    #[command(flatten)]
    pub identity: IdentityArgs,

    /// Run in light client mode (no chunk storage)
    #[arg(long)]
    pub light: bool,

    /// Run the node on the mainnet
    #[arg(long, conflicts_with = "testnet")]
    pub mainnet: bool,

    /// Run the node on the testnet
    #[arg(long, conflicts_with = "mainnet")]
    pub testnet: bool,
}

// =============================================================================
// Logging
// =============================================================================

/// Logging configuration
#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "Logging")]
pub struct LogArgs {
    /// Silence all output
    #[arg(short, long)]
    pub quiet: bool,

    /// Verbose mode (-v, -vv, -vvv, etc.)
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbosity: u8,

    /// Log filter directive (e.g., "vertex=debug,libp2p=info")
    #[arg(long = "log.filter", value_name = "DIRECTIVE")]
    pub filter: Option<String>,
}

// =============================================================================
// Data Directory
// =============================================================================

/// Data directory configuration
#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "Datadir")]
pub struct DataDirArgs {
    /// Data directory path
    #[arg(long, value_name = "PATH")]
    pub datadir: Option<PathBuf>,
}

// =============================================================================
// Identity
// =============================================================================

/// Identity and keystore configuration
#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "Identity")]
pub struct IdentityArgs {
    /// Password for keystore encryption/decryption.
    ///
    /// Can also be set via the VERTEX_PASSWORD environment variable.
    #[arg(long, env = "VERTEX_PASSWORD")]
    pub password: Option<String>,

    /// Path to file containing keystore password
    #[arg(long = "password-file")]
    pub password_file: Option<PathBuf>,

    /// Use ephemeral identity (random key, not persisted).
    ///
    /// Default for light nodes. For full nodes with redistribution or SWAP,
    /// using ephemeral identity means losing overlay address on restart.
    #[arg(long)]
    pub ephemeral: bool,
}

// =============================================================================
// Networking
// =============================================================================

/// Network configuration
#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "Networking")]
pub struct NetworkArgs {
    /// Disable the P2P discovery service
    #[arg(long = "network.no-discovery")]
    pub disable_discovery: bool,

    /// Comma-separated list of bootstrap node multiaddresses
    #[arg(long = "network.bootnodes", value_delimiter = ',')]
    pub bootnodes: Option<Vec<String>>,

    /// P2P listen port
    #[arg(long = "network.port", default_value_t = crate::constants::DEFAULT_P2P_PORT)]
    pub port: u16,

    /// P2P listen address
    #[arg(long = "network.addr", default_value = "0.0.0.0")]
    pub addr: String,

    /// Maximum number of peers
    #[arg(long = "network.max-peers", default_value_t = crate::constants::DEFAULT_MAX_PEERS)]
    pub max_peers: usize,
}

// =============================================================================
// Storage
// =============================================================================

/// Storage configuration
#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "Storage")]
pub struct StorageArgs {
    /// Maximum storage capacity in GB
    #[arg(long = "storage.capacity", default_value_t = crate::constants::DEFAULT_MAX_STORAGE_SIZE_GB)]
    pub capacity: u64,

    /// Participate in redistribution (requires persistent identity)
    #[arg(long)]
    pub redistribution: bool,

    /// Enable staking (requires persistent identity)
    #[arg(long)]
    pub staking: bool,
}

// =============================================================================
// API
// =============================================================================

/// API server configuration
#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "API")]
pub struct ApiArgs {
    /// Enable the HTTP API
    #[arg(long = "api.http")]
    pub http: bool,

    /// HTTP API listen address
    #[arg(long = "api.http-addr", default_value = "127.0.0.1")]
    pub http_addr: String,

    /// HTTP API listen port
    #[arg(long = "api.http-port", default_value_t = crate::constants::DEFAULT_HTTP_API_PORT)]
    pub http_port: u16,

    /// Enable metrics endpoint
    #[arg(long = "metrics")]
    pub metrics: bool,

    /// Metrics listen address
    #[arg(long = "metrics.addr", default_value = "127.0.0.1")]
    pub metrics_addr: String,

    /// Metrics listen port
    #[arg(long = "metrics.port", default_value_t = crate::constants::DEFAULT_METRICS_PORT)]
    pub metrics_port: u16,
}
