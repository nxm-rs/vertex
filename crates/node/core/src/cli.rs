//! Command-line interface for the Vertex Swarm node.
//!
//! Arguments are organized into logical groups that correspond to node subsystems.
//! See CLI_ARCHITECTURE.md for the full design.
//!
//! # Config Trait Implementations
//!
//! CLI argument structs implement the config traits from `vertex_swarm_api::config`,
//! allowing them to be passed directly to component builders without intermediate
//! conversion steps.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::time::Duration;
use vertex_bandwidth_core::{
    DEFAULT_BASE_PRICE, DEFAULT_EARLY_PAYMENT_PERCENT, DEFAULT_LIGHT_FACTOR,
    DEFAULT_PAYMENT_THRESHOLD, DEFAULT_PAYMENT_TOLERANCE_PERCENT, DEFAULT_REFRESH_RATE,
};
use vertex_swarm_api::{
    ApiConfig, AvailabilityIncentiveConfig, IdentityConfig, NetworkConfig, StorageConfig,
    StoreConfig,
};

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
    /// Node type determines what capabilities and protocols the node runs.
    ///
    /// - bootnode: Only topology (peer discovery)
    /// - light: Retrieve chunks (default)
    /// - publisher: Retrieve + upload chunks
    /// - full: Store chunks for network
    /// - staker: Full + redistribution rewards
    #[arg(long = "type", value_enum, default_value_t = NodeTypeCli::Light)]
    pub node_type: NodeTypeCli,

    /// Data directory configuration
    #[command(flatten)]
    pub datadir: DataDirArgs,

    /// Network configuration
    #[command(flatten)]
    pub network: NetworkArgs,

    /// Availability incentive configuration
    #[command(flatten)]
    pub availability: AvailabilityArgs,

    /// Local storage / cache configuration
    #[command(flatten)]
    pub storage: StorageArgs,

    /// Storage incentive configuration
    #[command(flatten)]
    pub storage_incentives: StorageIncentiveArgs,

    /// API configuration
    #[command(flatten)]
    pub api: ApiArgs,

    /// Identity configuration
    #[command(flatten)]
    pub identity: IdentityArgs,

    /// Run the node on the mainnet
    #[arg(long, conflicts_with_all = ["testnet", "swarmspec"])]
    pub mainnet: bool,

    /// Run the node on the testnet
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

/// Node type for CLI (maps to config::NodeType).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum NodeTypeCli {
    /// Only participates in topology (Kademlia/Hive)
    Bootnode,
    /// Can retrieve chunks from the network
    #[default]
    Light,
    /// Can retrieve + upload chunks
    Publisher,
    /// Stores chunks for the network
    Full,
    /// Full + redistribution game participation
    Staker,
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

    /// Nonce for overlay address derivation (hex-encoded, 32 bytes).
    ///
    /// The overlay address is derived as: keccak256(eth_address || network_id || nonce).
    /// Changing the nonce changes the node's position in the DHT.
    /// If not set, uses nonce from config file or generates a random one.
    #[arg(long, value_parser = parse_nonce)]
    pub nonce: Option<alloy_primitives::B256>,

    /// Use ephemeral identity (random key, not persisted).
    ///
    /// Ephemeral nodes lose their overlay address on restart.
    #[arg(long)]
    pub ephemeral: bool,
}

/// Parse a hex-encoded 32-byte nonce from CLI.
fn parse_nonce(s: &str) -> Result<alloy_primitives::B256, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).map_err(|e| format!("invalid hex: {}", e))?;
    if bytes.len() != 32 {
        return Err(format!("nonce must be 32 bytes, got {}", bytes.len()));
    }
    Ok(alloy_primitives::B256::from_slice(&bytes))
}

// =============================================================================
// Networking
// =============================================================================

/// P2P network configuration
#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "Networking")]
pub struct NetworkArgs {
    /// Disable the P2P discovery service
    #[arg(long = "network.no-discovery")]
    pub disable_discovery: bool,

    /// Comma-separated list of bootstrap node multiaddresses
    #[arg(long = "network.bootnodes", value_delimiter = ',')]
    pub bootnodes: Option<Vec<String>>,

    /// Comma-separated list of trusted peer multiaddresses to connect to on startup.
    ///
    /// Unlike bootnodes, trusted peers are regular nodes that the node will actively
    /// maintain connections with. Useful for connecting to known peers when bootnodes
    /// return no peer addresses (e.g., as a light node connecting to full nodes).
    ///
    /// Example: --network.trusted-peers /ip4/1.2.3.4/tcp/1634/p2p/QmPeer1,/ip4/5.6.7.8/tcp/1634/p2p/QmPeer2
    #[arg(long = "network.trusted-peers", value_delimiter = ',')]
    pub trusted_peers: Option<Vec<String>>,

    /// P2P listen port
    #[arg(long = "network.port", default_value_t = crate::constants::DEFAULT_P2P_PORT)]
    pub port: u16,

    /// P2P listen address
    #[arg(long = "network.addr", default_value = crate::constants::DEFAULT_LISTEN_ADDR)]
    pub addr: String,

    /// Maximum number of peers
    #[arg(long = "network.max-peers", default_value_t = crate::constants::DEFAULT_MAX_PEERS)]
    pub max_peers: usize,

    /// Connection idle timeout in seconds
    #[arg(long = "network.idle-timeout", default_value_t = crate::constants::DEFAULT_IDLE_TIMEOUT_SECS)]
    pub idle_timeout_secs: u64,
}

impl NetworkArgs {
    /// Get the primary listen address as a multiaddr string.
    fn listen_multiaddr(&self) -> String {
        format!("/ip4/{}/tcp/{}", self.addr, self.port)
    }
}

// =============================================================================
// Availability Incentives
// =============================================================================

/// Availability incentive mode
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum AvailabilityMode {
    /// No availability accounting (dev/testing only)
    None,
    /// Soft accounting without real payments
    #[default]
    Pseudosettle,
    /// Real payment channels with chequebook
    Swap,
    /// Both pseudosettle and SWAP (SWAP when threshold reached)
    Both,
}

/// Availability incentive configuration
///
/// All thresholds are in **Accounting Units (AU)**, matching Bee's accounting system.
/// Default values match Bee: payment threshold = 13,500,000 AU, tolerance = 25%.
#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "Availability Incentives")]
pub struct AvailabilityArgs {
    /// Availability incentive mode.
    ///
    /// - none: No accounting (dev only)
    /// - pseudosettle: Soft accounting without payments (default)
    /// - swap: Real payments via SWAP chequebook
    /// - both: Pseudosettle until threshold, then SWAP
    #[arg(long = "availability.mode", value_enum, default_value_t = AvailabilityMode::Pseudosettle)]
    pub mode: AvailabilityMode,

    /// Payment threshold in accounting units.
    ///
    /// When a peer's debt reaches this threshold, settlement is requested.
    /// Default: 13,500,000 AU (matches Bee).
    #[arg(long = "availability.threshold", default_value_t = DEFAULT_PAYMENT_THRESHOLD)]
    pub payment_threshold: u64,

    /// Payment tolerance as a percentage (0-100).
    ///
    /// Disconnect threshold = payment_threshold * (100 + tolerance) / 100.
    /// Default: 25% (matches Bee).
    #[arg(long = "availability.tolerance-percent", default_value_t = DEFAULT_PAYMENT_TOLERANCE_PERCENT)]
    pub payment_tolerance_percent: u64,

    /// Base price per chunk in accounting units.
    ///
    /// Actual price depends on proximity: (31 - proximity + 1) * base_price.
    /// Default: 10,000 AU (matches Bee).
    #[arg(long = "availability.base-price", default_value_t = DEFAULT_BASE_PRICE)]
    pub base_price: u64,

    /// Refresh rate in accounting units per second.
    ///
    /// Used for pseudosettle time-based allowance.
    /// Default: 4,500,000 AU/s for full nodes (matches Bee).
    #[arg(long = "availability.refresh-rate", default_value_t = DEFAULT_REFRESH_RATE)]
    pub refresh_rate: u64,

    /// Early payment trigger percentage (0-100).
    ///
    /// Settlement is triggered when debt exceeds (100 - early)% of threshold.
    /// Default: 50% (matches Bee).
    #[arg(long = "availability.early-percent", default_value_t = DEFAULT_EARLY_PAYMENT_PERCENT)]
    pub early_payment_percent: u64,

    /// Light node scaling factor.
    ///
    /// Light nodes have all thresholds and rates divided by this factor.
    /// Default: 10 (matches Bee).
    #[arg(long = "availability.light-factor", default_value_t = DEFAULT_LIGHT_FACTOR)]
    pub light_factor: u64,
}

// =============================================================================
// Local Storage / Cache
// =============================================================================

/// Local storage and cache configuration
#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "Local Storage / Cache")]
pub struct StorageArgs {
    /// Maximum storage capacity in number of chunks.
    ///
    /// Storage in Swarm is measured in chunks (typically 4KB each).
    /// Default is 2^22 chunks (~20GB with metadata).
    #[arg(long = "storage.chunks", default_value_t = vertex_swarmspec::DEFAULT_RESERVE_CAPACITY)]
    pub capacity_chunks: u64,

    /// Cache capacity in number of chunks.
    ///
    /// In-memory cache for frequently accessed chunks (Light/Publisher nodes).
    /// Default is 2^16 chunks (~256MB in memory).
    #[arg(long = "cache.chunks", default_value_t = vertex_swarmspec::DEFAULT_CACHE_CAPACITY)]
    pub cache_chunks: u64,
}

// =============================================================================
// Storage Incentives
// =============================================================================

/// Storage incentive configuration (redistribution, postage)
#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "Storage Incentives")]
pub struct StorageIncentiveArgs {
    /// Participate in redistribution (requires persistent identity and staking).
    ///
    /// When enabled, the node participates in the redistribution game to earn
    /// rewards for storing chunks in its neighborhood.
    #[arg(long)]
    pub redistribution: bool,
}

// =============================================================================
// API
// =============================================================================

/// API server configuration
#[derive(Debug, Args, Clone)]
#[command(next_help_heading = "API")]
pub struct ApiArgs {
    /// Enable the gRPC server
    #[arg(long = "grpc")]
    pub grpc: bool,

    /// gRPC server listen address
    #[arg(long = "grpc.addr", default_value = crate::constants::DEFAULT_LOCALHOST_ADDR)]
    pub grpc_addr: String,

    /// gRPC server listen port
    #[arg(long = "grpc.port", default_value_t = crate::constants::DEFAULT_GRPC_PORT)]
    pub grpc_port: u16,

    /// Enable metrics HTTP endpoint
    #[arg(long = "metrics")]
    pub metrics: bool,

    /// Metrics listen address
    #[arg(long = "metrics.addr", default_value = crate::constants::DEFAULT_LOCALHOST_ADDR)]
    pub metrics_addr: String,

    /// Metrics listen port
    #[arg(long = "metrics.port", default_value_t = crate::constants::DEFAULT_METRICS_PORT)]
    pub metrics_port: u16,
}

// =============================================================================
// Config Trait Implementations
// =============================================================================
//
// CLI argument structs implement the config traits from swarm-api, allowing
// them to be passed directly to component builders without conversion.

impl AvailabilityIncentiveConfig for AvailabilityArgs {
    fn pseudosettle_enabled(&self) -> bool {
        matches!(
            self.mode,
            AvailabilityMode::Pseudosettle | AvailabilityMode::Both
        )
    }

    fn swap_enabled(&self) -> bool {
        matches!(self.mode, AvailabilityMode::Swap | AvailabilityMode::Both)
    }

    fn payment_threshold(&self) -> u64 {
        self.payment_threshold
    }

    fn payment_tolerance_percent(&self) -> u64 {
        self.payment_tolerance_percent
    }

    fn base_price(&self) -> u64 {
        self.base_price
    }

    fn refresh_rate(&self) -> u64 {
        self.refresh_rate
    }

    fn early_payment_percent(&self) -> u64 {
        self.early_payment_percent
    }

    fn light_factor(&self) -> u64 {
        self.light_factor
    }
}

impl StoreConfig for StorageArgs {
    fn capacity_chunks(&self) -> u64 {
        self.capacity_chunks
    }

    fn cache_chunks(&self) -> u64 {
        self.cache_chunks
    }
}

impl StorageConfig for StorageIncentiveArgs {
    fn redistribution_enabled(&self) -> bool {
        self.redistribution
    }
}

impl ApiConfig for ApiArgs {
    fn grpc_enabled(&self) -> bool {
        self.grpc
    }

    fn grpc_addr(&self) -> &str {
        &self.grpc_addr
    }

    fn grpc_port(&self) -> u16 {
        self.grpc_port
    }

    fn metrics_enabled(&self) -> bool {
        self.metrics
    }

    fn metrics_addr(&self) -> &str {
        &self.metrics_addr
    }

    fn metrics_port(&self) -> u16 {
        self.metrics_port
    }
}

impl IdentityConfig for IdentityArgs {
    fn ephemeral(&self) -> bool {
        self.ephemeral
    }

    fn requires_persistent(&self) -> bool {
        // IdentityArgs alone cannot determine this - it depends on node type.
        // This returns a conservative default; the node command should check
        // node type to determine actual persistence requirements.
        !self.ephemeral
    }
}

impl NetworkConfig for NetworkArgs {
    fn listen_addrs(&self) -> Vec<String> {
        vec![self.listen_multiaddr()]
    }

    fn bootnodes(&self) -> Vec<String> {
        self.bootnodes.clone().unwrap_or_default()
    }

    fn discovery_enabled(&self) -> bool {
        !self.disable_discovery
    }

    fn max_peers(&self) -> usize {
        self.max_peers
    }

    fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout_secs)
    }
}
