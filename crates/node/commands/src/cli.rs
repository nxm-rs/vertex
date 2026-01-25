//! CLI argument assembly and top-level parser.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

// Re-export args from their respective crates
pub use vertex_node_core::args::{ApiArgs, DataDirArgs, DatabaseArgs, LogArgs};
pub use vertex_swarm_core::args::{
    AvailabilityArgs, AvailabilityMode, IdentityArgs, NetworkArgs, StorageArgs,
    StorageIncentiveArgs, SwarmNodeType,
};

/// Vertex Swarm - Ethereum Swarm Node Implementation
#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Logging configuration.
    #[command(flatten)]
    pub logs: LogArgs,

    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: Commands,
}

/// Swarm node commands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run a Swarm node.
    Node(NodeArgs),
}

/// Arguments for the 'node' command.
#[derive(Debug, Args)]
pub struct NodeArgs {
    /// Node type determines what capabilities and protocols the node runs.
    ///
    /// - bootnode: Only topology (peer discovery)
    /// - light: Retrieve chunks (default)
    /// - publisher: Retrieve + upload chunks
    /// - full: Store chunks for network
    /// - staker: Full + redistribution rewards
    #[arg(long = "type", value_enum, default_value_t = SwarmNodeType::Light)]
    pub node_type: SwarmNodeType,

    /// Data directory configuration.
    #[command(flatten)]
    pub datadir: DataDirArgs,

    /// Database configuration.
    #[command(flatten)]
    pub database: DatabaseArgs,

    /// Network configuration.
    #[command(flatten)]
    pub network: NetworkArgs,

    /// Availability incentive configuration.
    #[command(flatten)]
    pub availability: AvailabilityArgs,

    /// Local storage / cache configuration.
    #[command(flatten)]
    pub storage: StorageArgs,

    /// Storage incentive configuration.
    #[command(flatten)]
    pub storage_incentives: StorageIncentiveArgs,

    /// API configuration.
    #[command(flatten)]
    pub api: ApiArgs,

    /// Identity configuration.
    #[command(flatten)]
    pub identity: IdentityArgs,

    /// Run the node on the mainnet.
    #[arg(long, conflicts_with_all = ["testnet", "swarmspec"])]
    pub mainnet: bool,

    /// Run the node on the testnet.
    #[arg(long, conflicts_with_all = ["mainnet", "swarmspec"])]
    pub testnet: bool,

    /// Path to a custom SwarmSpec file (JSON/TOML) for local/dev networks.
    ///
    /// The SwarmSpec defines the complete network configuration including:
    /// - network_id: The network identifier
    /// - network_name: Human-readable network name
    /// - bootnodes: List of bootnode multiaddrs
    ///
    /// Cannot be used with --mainnet or --testnet.
    #[arg(long, conflicts_with_all = ["mainnet", "testnet"], value_name = "PATH")]
    pub swarmspec: Option<PathBuf>,
}
