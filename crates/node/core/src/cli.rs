//! Command-line interface for the Vertex Swarm node.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// Vertex Swarm - Ethereum Swarm Node Implementation
#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Logging configuration
    #[command(flatten)]
    pub log_args: LogArgs,

    /// Subcommand to execute
    #[command(subcommand)]
    pub command: Commands,
}

/// Logging configuration
#[derive(Debug, Args, Clone)]
pub struct LogArgs {
    /// Silence all output
    #[arg(short, long)]
    pub quiet: bool,

    /// Verbose mode (-v, -vv, -vvv, etc.)
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbosity: u8,

    /// Include timestamps in logs
    #[arg(long)]
    pub timestamps: bool,

    /// Enable logging to file
    #[arg(long)]
    pub log_file: bool,

    /// Log file directory
    #[arg(long)]
    pub log_dir: Option<PathBuf>,

    /// Log filter
    #[arg(long, value_name = "DIRECTIVE")]
    pub filter: Option<String>,
}

/// Swarm node commands
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run a Swarm node
    Node(NodeArgs),

    /// Run a development Swarm node
    Dev(DevArgs),

    /// Display information about the node
    Info(InfoArgs),

    /// Manage node configuration
    Config(ConfigArgs),
}

/// Identity configuration
#[derive(Debug, Args, Clone)]
pub struct IdentityArgs {
    /// Password for keystore encryption/decryption.
    ///
    /// Can also be set via the VERTEX_PASSWORD environment variable.
    #[arg(long, env = "VERTEX_PASSWORD")]
    pub password: Option<String>,

    /// Path to file containing keystore password
    #[arg(long)]
    pub password_file: Option<std::path::PathBuf>,

    /// Use ephemeral identity (random key, not persisted).
    ///
    /// Default for light nodes. For full nodes with redistribution or SWAP,
    /// using ephemeral identity means losing overlay address on restart.
    #[arg(long)]
    pub ephemeral: bool,
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

/// Arguments for the 'dev' command
#[derive(Debug, Args)]
pub struct DevArgs {
    /// Data directory configuration
    #[command(flatten)]
    pub datadir: DataDirArgs,

    /// API configuration
    #[command(flatten)]
    pub api: ApiArgs,

    /// Block time interval in seconds (0 means instant mining)
    #[arg(long, default_value = "0")]
    pub block_time: u64,

    /// Number of accounts to generate
    #[arg(long, default_value = "10")]
    pub accounts: u8,

    /// Amount of test BZZ to prefund accounts with
    #[arg(long, default_value = "1000")]
    pub prefund_amount: u64,
}

/// Arguments for the 'info' command
#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Data directory configuration
    #[command(flatten)]
    pub datadir: DataDirArgs,

    /// Show network information
    #[arg(long)]
    pub network: bool,

    /// Show storage information
    #[arg(long)]
    pub storage: bool,

    /// Show peer information
    #[arg(long)]
    pub peers: bool,

    /// Show all information
    #[arg(short, long)]
    pub all: bool,
}

/// Arguments for the 'config' command
#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// Data directory configuration
    #[command(flatten)]
    pub datadir: DataDirArgs,

    /// Initialize default configuration
    #[arg(long)]
    pub init: bool,

    /// Show current configuration
    #[arg(long)]
    pub show: bool,

    /// Set configuration value (key=value)
    #[arg(long)]
    pub set: Option<String>,
}

/// Data directory configuration
#[derive(Debug, Args, Clone)]
pub struct DataDirArgs {
    /// Data directory path
    #[arg(long, value_name = "PATH")]
    pub datadir: Option<PathBuf>,

    /// Path to static files
    #[arg(long, value_name = "PATH")]
    pub static_files_path: Option<PathBuf>,
}

/// Network configuration
#[derive(Debug, Args)]
pub struct NetworkArgs {
    /// Disable the discovery service
    #[arg(long)]
    pub disable_discovery: bool,

    /// Comma-separated list of bootstrap nodes
    #[arg(long, value_delimiter = ',')]
    pub bootnodes: Option<Vec<String>>,

    /// The network port to listen on
    #[arg(long, default_value_t = crate::constants::DEFAULT_P2P_PORT)]
    pub port: u16,

    /// The network address to listen on
    #[arg(long, default_value = "0.0.0.0")]
    pub addr: String,

    /// Maximum number of peers
    #[arg(long, default_value_t = crate::constants::DEFAULT_MAX_PEERS)]
    pub max_peers: usize,

    /// NAT port mapping mechanism (none, upnp, pmp)
    #[arg(long, value_name = "MECHANISM", default_value = "upnp")]
    pub nat: String,

    /// Connect to trusted peers only
    #[arg(long)]
    pub trusted_only: bool,

    /// Comma-separated list of trusted peer multiaddresses
    #[arg(long, value_delimiter = ',')]
    pub trusted_peers: Option<Vec<String>>,
}

/// Storage configuration
#[derive(Debug, Args)]
pub struct StorageArgs {
    /// Maximum storage size in GB
    #[arg(long, default_value_t = crate::constants::DEFAULT_MAX_STORAGE_SIZE_GB)]
    pub max_storage: u64,

    /// Maximum number of chunks to store
    #[arg(long, default_value_t = crate::constants::DEFAULT_MAX_CHUNKS as u64)]
    pub max_chunks: u64,

    /// Participate in redistribution lottery
    #[arg(long)]
    pub redistribution: bool,

    /// Enable staking
    #[arg(long)]
    pub staking: bool,
}

/// API configuration
#[derive(Debug, Args, Clone)]
pub struct ApiArgs {
    /// Enable the HTTP API
    #[arg(long)]
    pub http: bool,

    /// HTTP API address
    #[arg(long, default_value = "127.0.0.1")]
    pub http_addr: String,

    /// HTTP API port
    #[arg(long, default_value_t = crate::constants::DEFAULT_HTTP_API_PORT)]
    pub http_port: u16,

    /// Enable metrics endpoint
    #[arg(long)]
    pub metrics: bool,

    /// Metrics address
    #[arg(long, default_value = "127.0.0.1")]
    pub metrics_addr: String,

    /// Metrics port
    #[arg(long, default_value_t = crate::constants::DEFAULT_METRICS_PORT)]
    pub metrics_port: u16,

    /// Enable CORS for HTTP API (comma-separated list of origins)
    #[arg(long)]
    pub cors: Option<String>,

    /// Enable authentication for APIs
    #[arg(long)]
    pub auth: bool,
}
