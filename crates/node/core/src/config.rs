//! Generic configuration loading for Vertex nodes.
//!
//! This module provides [`FullNodeConfig<P>`], a generic configuration struct
//! that combines node infrastructure settings with protocol-specific configuration.
//!
//! # Configuration Hierarchy
//!
//! Configuration is loaded with the following priority (highest wins):
//!
//! 1. CLI arguments (applied via `apply_args()`)
//! 2. Config file (TOML)
//! 3. Environment variables (`VERTEX_` prefix)
//! 4. Defaults
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_core::config::FullNodeConfig;
//! use vertex_swarm_node::ProtocolConfig;
//!
//! // Load combined config
//! let mut config = FullNodeConfig::<ProtocolConfig>::load(Some(&config_path))?;
//!
//! // Apply CLI overrides
//! config.apply_args(&node_args, &swarm_args);
//! ```

use std::path::Path;

use eyre::{Result, WrapErr};
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use vertex_node_api::NodeProtocolConfig;

use crate::args::{ApiArgs, DatabaseArgs, InfraArgs};

/// Infrastructure configuration (generic, protocol-agnostic).
///
/// This contains settings for node-level infrastructure like API servers,
/// database, etc. It does not include any protocol-specific settings.
/// Logging is handled separately at the CLI level.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InfraConfig {
    /// API server configuration (gRPC, metrics).
    pub api: ApiArgs,

    /// Database configuration.
    pub database: DatabaseArgs,
}

impl InfraConfig {
    /// Apply CLI argument overrides.
    pub fn apply_args(&mut self, args: &InfraArgs) {
        self.api = args.api.clone();
        self.database = args.database.clone();
    }
}

/// Full node configuration combining infrastructure and protocol settings.
///
/// This is the main configuration type used by the node. It is generic over
/// the protocol config type `P`, allowing different protocols to define their
/// own configuration structure.
///
/// # Type Parameters
///
/// - `P`: Protocol configuration type implementing [`NodeProtocolConfig`]
///
/// # Serialization
///
/// The configuration is serialized as a flat structure where infrastructure
/// and protocol fields are at the same level:
///
/// ```toml
/// # Infrastructure (from InfraConfig)
/// [api]
/// grpc = true
/// grpc_port = 5555
///
/// [database]
/// memory_only = false
///
/// # Protocol-specific (from P, e.g., ProtocolConfig)
/// node_type = "light"
///
/// [network]
/// port = 1634
/// max_peers = 50
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FullNodeConfig<P>
where
    P: Default,
{
    /// Infrastructure configuration.
    #[serde(flatten)]
    pub infra: InfraConfig,

    /// Protocol-specific configuration.
    #[serde(flatten)]
    pub protocol: P,
}

impl<P: Default> Default for FullNodeConfig<P> {
    fn default() -> Self {
        Self {
            infra: InfraConfig::default(),
            protocol: P::default(),
        }
    }
}

impl<P> FullNodeConfig<P>
where
    P: NodeProtocolConfig + Serialize + DeserializeOwned,
{
    /// Load configuration from defaults, environment, and optional config file.
    ///
    /// Configuration sources are merged with the following priority (highest wins):
    /// 1. Config file (if provided and exists)
    /// 2. Environment variables (`VERTEX_` prefix, `_` as separator)
    /// 3. Defaults
    ///
    /// CLI argument overrides should be applied separately using [`Self::apply_args`].
    pub fn load(config_path: Option<&Path>) -> Result<Self> {
        let mut figment = Figment::new()
            .merge(Serialized::defaults(Self::default()))
            .merge(Env::prefixed("VERTEX_").split("_"));

        if let Some(path) = config_path
            && path.exists()
        {
            figment = figment.merge(Toml::file(path));
        }

        figment.extract().wrap_err("Failed to load configuration")
    }

    /// Apply CLI argument overrides to this configuration.
    ///
    /// This should be called after [`Self::load`] to apply command-line overrides.
    pub fn apply_args(&mut self, infra_args: &InfraArgs, protocol_args: &P::Args) {
        self.infra.apply_args(infra_args);
        self.protocol.apply_args(protocol_args);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal test protocol config
    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
    struct TestNodeProtocolConfig {
        test_value: u32,
    }

    #[derive(Clone)]
    struct TestArgs {
        test_value: u32,
    }

    impl NodeProtocolConfig for TestNodeProtocolConfig {
        type Args = TestArgs;

        fn apply_args(&mut self, args: &Self::Args) {
            self.test_value = args.test_value;
        }
    }

    #[test]
    fn test_default_config() {
        let config = FullNodeConfig::<TestNodeProtocolConfig>::default();
        assert!(!config.infra.api.grpc);
        assert_eq!(config.protocol.test_value, 0);
    }

    #[test]
    fn test_load_missing_file_uses_defaults() {
        let config =
            FullNodeConfig::<TestNodeProtocolConfig>::load(Some(Path::new("/nonexistent.toml")))
                .unwrap();
        assert!(!config.infra.api.grpc);
        assert_eq!(config.protocol.test_value, 0);
    }
}
