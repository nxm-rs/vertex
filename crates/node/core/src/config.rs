//! Node configuration handling.

use crate::{
    cli::{ApiArgs, NetworkArgs, StorageArgs},
    constants::*,
};
use eyre::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::{IpAddr, SocketAddr},
    path::Path,
    str::FromStr,
};
use vertex_swarmspec::Hive;

/// Configuration for the Vertex Swarm node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Network configuration
    pub network: NetworkConfig,

    /// Storage configuration
    pub storage: StorageConfig,

    /// Bandwidth configuration
    pub bandwidth: BandwidthConfig,

    /// API configuration
    pub api: ApiConfig,

    /// Node mode (light, full, incentivized)
    #[serde(default)]
    pub mode: NodeMode,
}

/// Node operation mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeMode {
    /// Light client mode (no chunk storage)
    Light,

    /// Full node mode (stores chunks)
    #[default]
    Full,

    /// Incentivized node (participates in redistribution)
    Incentivized,
}

/// Network configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Whether to enable peer discovery
    #[serde(default = "default_discovery")]
    pub discovery: bool,

    /// Bootstrap nodes
    #[serde(default)]
    pub bootnodes: Vec<String>,

    /// Listening address
    #[serde(default = "default_addr")]
    pub addr: IpAddr,

    /// Listening port
    #[serde(default = "default_port")]
    pub port: u16,

    /// Maximum number of peers
    #[serde(default = "default_max_peers")]
    pub max_peers: usize,

    /// NAT traversal method
    #[serde(default = "default_nat")]
    pub nat: String,

    /// Connect to trusted peers only
    #[serde(default)]
    pub trusted_only: bool,

    /// Trusted peers
    #[serde(default)]
    pub trusted_peers: Vec<String>,
}

/// Storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Maximum storage size in bytes
    #[serde(default = "default_max_storage")]
    pub max_storage: u64,

    /// Maximum number of chunks
    #[serde(default = "default_max_chunks")]
    pub max_chunks: u64,

    /// Target number of chunks
    #[serde(default = "default_target_chunks")]
    pub target_chunks: u64,

    /// Minimum number of chunks
    #[serde(default = "default_min_chunks")]
    pub min_chunks: u64,

    /// Reserve percentage
    #[serde(default = "default_reserve_percentage")]
    pub reserve_percentage: u8,

    /// Whether to participate in redistribution
    #[serde(default)]
    pub redistribution: bool,

    /// Whether to participate in staking
    #[serde(default)]
    pub staking: bool,
}

/// Bandwidth configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandwidthConfig {
    /// Whether to use pseudosettle
    #[serde(default = "default_true")]
    pub pseudosettle_enabled: bool,

    /// Whether to use SWAP payment channels
    #[serde(default)]
    pub swap_enabled: bool,

    /// Daily free bandwidth allowance in bytes
    #[serde(default = "default_daily_bandwidth")]
    pub daily_allowance: u64,

    /// Payment threshold in bytes
    #[serde(default = "default_payment_threshold")]
    pub payment_threshold: u64,

    /// Payment tolerance in bytes
    #[serde(default = "default_payment_tolerance")]
    pub payment_tolerance: u64,

    /// Disconnect threshold in bytes
    #[serde(default = "default_disconnect_threshold")]
    pub disconnect_threshold: u64,

    /// Price per byte in base units
    #[serde(default = "default_price_per_byte")]
    pub price_per_byte: u64,
}

/// API configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// Whether to enable HTTP API
    #[serde(default)]
    pub http_enabled: bool,

    /// HTTP API address
    #[serde(default = "default_http_addr")]
    pub http_addr: String,

    /// HTTP API port
    #[serde(default = "default_http_port")]
    pub http_port: u16,

    /// Whether to enable metrics
    #[serde(default)]
    pub metrics_enabled: bool,

    /// Metrics address
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,

    /// Metrics port
    #[serde(default = "default_metrics_port")]
    pub metrics_port: u16,

    /// CORS domains
    #[serde(default)]
    pub cors: Option<String>,

    /// Whether to enable authentication
    #[serde(default)]
    pub auth_enabled: bool,
}

impl NodeConfig {
    /// Create a new default configuration
    pub fn new(network_spec: &Hive, mode: NodeMode) -> Self {
        Self {
            network: NetworkConfig {
                discovery: true,
                bootnodes: network_spec
                    .bootnodes
                    .iter()
                    .map(|addr| addr.to_string())
                    .collect(),
                addr: IpAddr::from_str("0.0.0.0").unwrap(),
                port: DEFAULT_P2P_PORT,
                max_peers: DEFAULT_MAX_PEERS,
                nat: "upnp".to_string(),
                trusted_only: false,
                trusted_peers: Vec::new(),
            },
            storage: StorageConfig {
                max_storage: DEFAULT_MAX_STORAGE_SIZE_GB * GB_TO_BYTES,
                max_chunks: DEFAULT_MAX_CHUNKS as u64,
                target_chunks: DEFAULT_TARGET_CHUNKS as u64,
                min_chunks: DEFAULT_MIN_CHUNKS as u64,
                reserve_percentage: DEFAULT_RESERVE_PERCENTAGE,
                redistribution: mode == NodeMode::Incentivized,
                staking: mode == NodeMode::Incentivized,
            },
            bandwidth: BandwidthConfig {
                pseudosettle_enabled: true,
                swap_enabled: false,
                daily_allowance: DEFAULT_DAILY_BANDWIDTH_ALLOWANCE,
                payment_threshold: DEFAULT_PAYMENT_THRESHOLD,
                payment_tolerance: DEFAULT_PAYMENT_TOLERANCE,
                disconnect_threshold: DEFAULT_DISCONNECT_THRESHOLD,
                price_per_byte: 10, // Default price per byte
            },
            api: ApiConfig {
                http_enabled: false,
                http_addr: "127.0.0.1".to_string(),
                http_port: DEFAULT_HTTP_API_PORT,
                metrics_enabled: false,
                metrics_addr: "127.0.0.1".to_string(),
                metrics_port: DEFAULT_METRICS_PORT,
                cors: None,
                auth_enabled: false,
            },
            mode,
        }
    }

    /// Load the configuration from the given path, or create a default one if it doesn't exist
    pub fn load_or_create(
        path: impl AsRef<Path>,
        network_spec: &Hive,
        mode: NodeMode,
    ) -> Result<Self> {
        let path = path.as_ref();

        if path.exists() {
            let content = fs::read_to_string(path)?;
            let config: Self = toml::from_str(&content)?;
            Ok(config)
        } else {
            let config = Self::new(network_spec, mode);
            config.save(path)?;
            Ok(config)
        }
    }

    /// Save the configuration to the given path
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)?;
        fs::write(path, content)?;

        Ok(())
    }

    /// Apply command line arguments to override the configuration
    pub fn apply_cli_args(
        &mut self,
        network_args: &NetworkArgs,
        storage_args: &StorageArgs,
        api_args: &ApiArgs,
        light: bool,
    ) {
        // Apply network args
        self.network.discovery = !network_args.disable_discovery;
        if let Some(bootnodes) = &network_args.bootnodes {
            self.network.bootnodes = bootnodes.clone();
        }
        self.network.addr = IpAddr::from_str(&network_args.addr).unwrap_or(self.network.addr);
        self.network.port = network_args.port;
        self.network.max_peers = network_args.max_peers;

        // Apply storage args
        self.storage.max_storage = storage_args.capacity * GB_TO_BYTES;
        self.storage.redistribution = storage_args.redistribution;
        self.storage.staking = storage_args.staking;

        // Apply node mode
        if light {
            self.mode = NodeMode::Light;
        } else if storage_args.redistribution {
            self.mode = NodeMode::Incentivized;
        }

        // Apply API args
        self.api.http_enabled = api_args.http;
        self.api.http_addr = api_args.http_addr.clone();
        self.api.http_port = api_args.http_port;

        self.api.metrics_enabled = api_args.metrics;
        self.api.metrics_addr = api_args.metrics_addr.clone();
        self.api.metrics_port = api_args.metrics_port;
    }

    /// Get the HTTP API socket address
    pub fn http_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(
            IpAddr::from_str(&self.api.http_addr)
                .unwrap_or_else(|_| IpAddr::from_str("127.0.0.1").unwrap()),
            self.api.http_port,
        )
    }

    /// Get the metrics socket address
    pub fn metrics_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(
            IpAddr::from_str(&self.api.metrics_addr)
                .unwrap_or_else(|_| IpAddr::from_str("127.0.0.1").unwrap()),
            self.api.metrics_port,
        )
    }

    /// Get the P2P socket address
    pub fn p2p_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.network.addr, self.network.port)
    }
}

// Default value functions

fn default_discovery() -> bool {
    true
}

fn default_addr() -> IpAddr {
    IpAddr::from_str("0.0.0.0").unwrap()
}

fn default_port() -> u16 {
    DEFAULT_P2P_PORT
}

fn default_max_peers() -> usize {
    DEFAULT_MAX_PEERS
}

fn default_nat() -> String {
    "upnp".to_string()
}

fn default_max_storage() -> u64 {
    DEFAULT_MAX_STORAGE_SIZE_GB * GB_TO_BYTES
}

fn default_max_chunks() -> u64 {
    DEFAULT_MAX_CHUNKS as u64
}

fn default_target_chunks() -> u64 {
    DEFAULT_TARGET_CHUNKS as u64
}

fn default_min_chunks() -> u64 {
    DEFAULT_MIN_CHUNKS as u64
}

fn default_reserve_percentage() -> u8 {
    DEFAULT_RESERVE_PERCENTAGE
}

fn default_true() -> bool {
    true
}

fn default_daily_bandwidth() -> u64 {
    DEFAULT_DAILY_BANDWIDTH_ALLOWANCE
}

fn default_payment_threshold() -> u64 {
    DEFAULT_PAYMENT_THRESHOLD
}

fn default_payment_tolerance() -> u64 {
    DEFAULT_PAYMENT_TOLERANCE
}

fn default_disconnect_threshold() -> u64 {
    DEFAULT_DISCONNECT_THRESHOLD
}

fn default_price_per_byte() -> u64 {
    10
}

fn default_http_addr() -> String {
    "127.0.0.1".to_string()
}

fn default_http_port() -> u16 {
    DEFAULT_HTTP_API_PORT
}

fn default_metrics_addr() -> String {
    "127.0.0.1".to_string()
}

fn default_metrics_port() -> u16 {
    DEFAULT_METRICS_PORT
}
