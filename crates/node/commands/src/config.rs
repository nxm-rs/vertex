//! Figment-based configuration loading.
//!
//! Configuration priority (highest wins):
//! 1. CLI arguments (applied after Figment load)
//! 2. Config file (TOML)
//! 3. Environment variables (`VERTEX_` prefix)
//! 4. Defaults

use crate::cli::{
    ApiArgs, AvailabilityArgs, AvailabilityMode, DatabaseArgs, IdentityArgs, NetworkArgs,
    StorageArgs, StorageIncentiveArgs, SwarmNodeType,
};
use eyre::{Result, WrapErr};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
};

/// Complete node configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    /// Node type (determines capabilities).
    pub node_type: SwarmNodeType,

    /// Network configuration.
    pub network: NetworkArgs,

    /// Availability incentive configuration.
    pub availability: AvailabilityArgs,

    /// Storage configuration.
    pub storage: StorageArgs,

    /// Storage incentive configuration.
    pub storage_incentives: StorageIncentiveArgs,

    /// API configuration.
    pub api: ApiArgs,

    /// Identity configuration.
    pub identity: IdentityArgs,

    /// Database configuration.
    pub database: DatabaseArgs,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            node_type: SwarmNodeType::default(),
            network: NetworkArgs::default(),
            availability: AvailabilityArgs::default(),
            storage: StorageArgs::default(),
            storage_incentives: StorageIncentiveArgs::default(),
            api: ApiArgs::default(),
            identity: IdentityArgs::default(),
            database: DatabaseArgs::default(),
        }
    }
}

impl NodeConfig {
    /// Load configuration from defaults, environment, and config file.
    /// CLI overrides should be applied separately after loading.
    pub fn load(config_path: Option<&Path>) -> Result<Self> {
        let mut figment = Figment::new()
            .merge(Serialized::defaults(NodeConfig::default()))
            .merge(Env::prefixed("VERTEX_").split("_"));

        if let Some(path) = config_path {
            if path.exists() {
                figment = figment.merge(Toml::file(path));
            }
        }

        figment.extract().wrap_err("Failed to load configuration")
    }

    /// Get the gRPC server socket address.
    pub fn grpc_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(
            self.api
                .grpc_addr
                .parse()
                .unwrap_or(IpAddr::from([127, 0, 0, 1])),
            self.api.grpc_port,
        )
    }

    /// Get the metrics socket address.
    pub fn metrics_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(
            self.api
                .metrics_addr
                .parse()
                .unwrap_or(IpAddr::from([127, 0, 0, 1])),
            self.api.metrics_port,
        )
    }

    /// Get the P2P listen address as a multiaddr string.
    pub fn p2p_listen_multiaddr(&self) -> String {
        self.network.listen_multiaddr()
    }

    /// Get the availability mode.
    pub fn availability_mode(&self) -> &AvailabilityMode {
        &self.availability.mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = NodeConfig::default();
        assert_eq!(config.node_type, SwarmNodeType::Light);
        assert!(!config.network.disable_discovery);
    }

    #[test]
    fn test_load_from_file() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.toml");

        // Write a config file
        fs::write(
            &config_path,
            r#"
node_type = "full"

[network]
max_peers = 100
"#,
        )
        .unwrap();

        let config = NodeConfig::load(Some(&config_path)).unwrap();
        assert_eq!(config.node_type, SwarmNodeType::Full);
        assert_eq!(config.network.max_peers, 100);
    }

    #[test]
    fn test_load_missing_file_uses_defaults() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("nonexistent.toml");

        let config = NodeConfig::load(Some(&config_path)).unwrap();
        assert_eq!(config.node_type, SwarmNodeType::Light);
        assert_eq!(config.network.max_peers, 50);
    }
}
