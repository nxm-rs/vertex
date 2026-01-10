//! Node configuration utilities

use std::path::PathBuf;

use vertex_primitives::Result;
use vertex_swarm_api::node::{NodeConfig, NodeMode};
use vertex_swarmspec::{mainnet, testnet, NetworkSpec, SwarmSpec};

/// Configuration builder for Swarm node
pub struct ConfigBuilder {
    /// Current configuration
    config: NodeConfig,
}

impl ConfigBuilder {
    /// Create a new configuration builder with mainnet settings
    pub fn mainnet() -> Self {
        Self {
            config: NodeConfig {
                mode: NodeMode::Light,
                network_id: mainnet::NETWORK_ID,
                network: Default::default(),
                storage: Default::default(),
                api_endpoint: "127.0.0.1:8546".to_string(),
                metrics_endpoint: Some("127.0.0.1:9091".to_string()),
                ..Default::default()
            },
        }
    }

    /// Create a new configuration builder with testnet settings
    pub fn testnet() -> Self {
        Self {
            config: NodeConfig {
                mode: NodeMode::Light,
                network_id: testnet::NETWORK_ID,
                network: Default::default(),
                storage: Default::default(),
                api_endpoint: "127.0.0.1:8546".to_string(),
                metrics_endpoint: Some("127.0.0.1:9091".to_string()),
                ..Default::default()
            },
        }
    }

    /// Create a new configuration builder with dev settings
    pub fn dev() -> Self {
        Self {
            config: NodeConfig {
                mode: NodeMode::Light,
                network_id: 1337, // dev network id
                network: Default::default(),
                storage: Default::default(),
                api_endpoint: "127.0.0.1:8546".to_string(),
                metrics_endpoint: Some("127.0.0.1:9091".to_string()),
                ..Default::default()
            },
        }
    }

    /// Configure as light node
    pub fn light(mut self) -> Self {
        self.config.mode = NodeMode::Light;
        self
    }

    /// Configure as full node
    pub fn full(mut self) -> Self {
        self.config.mode = NodeMode::Full;
        self
    }

    /// Configure as incentivized node
    pub fn incentivized(mut self) -> Self {
        self.config.mode = NodeMode::Incentivized;
        self
    }

    /// Set data directory
    pub fn data_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.storage.data_dir = path.into().to_string_lossy().to_string();
        self
    }

    /// Set API endpoint
    pub fn api_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.api_endpoint = endpoint.into();
        self
    }

    /// Enable or disable metrics
    pub fn metrics(mut self, enabled: bool, endpoint: Option<impl Into<String>>) -> Self {
        self.config.metrics_endpoint = enabled.then(|| {
            endpoint
                .map(|e| e.into())
                .unwrap_or_else(|| "127.0.0.1:9091".to_string())
        });
        self
    }

    /// Set maximum storage space
    pub fn max_storage(mut self, max_space: u64) -> Self {
        self.config.storage.max_space = max_space;
        self
    }

    /// Add a bootnode
    pub fn add_bootnode(mut self, addr: impl Into<String>) -> Self {
        self.config.network.bootnodes.push(addr.into());
        self
    }

    /// Build the configuration
    pub fn build(self) -> NodeConfig {
        self.config
    }

    /// Load configuration from a file
    pub fn load_from_file(path: impl Into<PathBuf>) -> Result<NodeConfig> {
        let path = path.into();

        // In a real implementation, this would read and parse a config file
        // For simplicity, we'll just return an error

        Err(vertex_primitives::Error::other(format!(
            "Loading config from file not implemented: {:?}",
            path
        )))
    }

    /// Save configuration to a file
    pub fn save_to_file(config: &NodeConfig, path: impl Into<PathBuf>) -> Result<()> {
        let path = path.into();

        // In a real implementation, this would serialize and write the config
        // For simplicity, we'll just return an error

        Err(vertex_primitives::Error::other(format!(
            "Saving config to file not implemented: {:?}",
            path
        )))
    }
}
