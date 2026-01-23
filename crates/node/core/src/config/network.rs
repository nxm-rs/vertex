//! Network configuration for TOML persistence.

use crate::constants::*;
use serde::{Deserialize, Serialize};
use std::{net::IpAddr, str::FromStr};

/// Network configuration (TOML-serializable).
///
/// This is the user-facing network configuration that persists to disk.
/// It gets converted to `P2PConfig` at runtime for use by the network module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Whether to enable peer discovery
    #[serde(default = "default_discovery")]
    pub discovery: bool,

    /// Bootstrap nodes (as string multiaddresses)
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

    /// Path to the peers database file (default: <datadir>/state/peers.json)
    #[serde(default)]
    pub peers_file: Option<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            discovery: default_discovery(),
            bootnodes: Vec::new(),
            addr: default_addr(),
            port: default_port(),
            max_peers: default_max_peers(),
            nat: default_nat(),
            trusted_only: false,
            trusted_peers: Vec::new(),
            peers_file: None,
        }
    }
}

fn default_discovery() -> bool {
    true
}

fn default_addr() -> IpAddr {
    IpAddr::from_str(DEFAULT_LISTEN_ADDR).unwrap()
}

fn default_port() -> u16 {
    DEFAULT_P2P_PORT
}

fn default_max_peers() -> usize {
    DEFAULT_MAX_PEERS
}

fn default_nat() -> String {
    DEFAULT_NAT_METHOD.to_string()
}
