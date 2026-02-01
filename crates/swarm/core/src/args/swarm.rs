//! Aggregated Swarm protocol CLI arguments.
//!
//! [`SwarmArgs`] combines all Swarm-specific configuration into a single
//! struct that can be flattened into a CLI parser. This is designed to be
//! composed with generic node arguments from `vertex-node-core`.
//!
//! # Example
//!
//! ```ignore
//! use clap::Parser;
//! use vertex_node_core::args::NodeArgs;
//! use vertex_swarm_core::args::SwarmArgs;
//!
//! #[derive(Parser)]
//! struct Cli {
//!     #[command(flatten)]
//!     node: NodeArgs,      // Generic infrastructure
//!
//!     #[command(flatten)]
//!     swarm: SwarmArgs,    // Protocol-specific
//! }
//! ```

use std::path::PathBuf;

use clap::Args;
use serde::{Deserialize, Serialize};

use vertex_swarm_primitives::SwarmNodeType;

use vertex_swarm_localstore::LocalStoreArgs;

use super::{BandwidthArgs, IdentityArgs, NetworkArgs, PricingArgs, StorageIncentiveArgs};

/// CLI argument type for node mode selection.
///
/// Maps to [`SwarmNodeType`]. Use `.into()` for conversion.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum, strum::FromRepr, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum NodeTypeArg {
    /// Topology only: peer discovery, no pricing or accounting.
    Bootnode = 0,
    /// Read + write: retrieval, pushsync, configurable accounting.
    #[default]
    Client = 1,
    /// Storage + staking: pullsync, local storage, redistribution.
    Storer = 2,
}

impl From<NodeTypeArg> for SwarmNodeType {
    fn from(arg: NodeTypeArg) -> Self {
        SwarmNodeType::from_repr(arg as u8).expect("matching repr")
    }
}

impl From<SwarmNodeType> for NodeTypeArg {
    fn from(node_type: SwarmNodeType) -> Self {
        NodeTypeArg::from_repr(node_type as u8).expect("matching repr")
    }
}

/// Aggregated Swarm protocol arguments.
///
/// This struct combines all Swarm-specific CLI arguments into a single
/// flattened group. It's designed to be composed with [`vertex_node_core::args::NodeArgs`]
/// in the binary's CLI parser.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SwarmArgs {
    /// Node mode: bootnode (topology only), client (read+write), storer (storage+staking).
    #[arg(long = "mode", value_enum, default_value_t = NodeTypeArg::Client)]
    pub node_type: NodeTypeArg,

    /// Network configuration.
    #[command(flatten)]
    pub network: NetworkArgs,

    /// Bandwidth accounting configuration.
    #[command(flatten)]
    pub bandwidth: BandwidthArgs,

    /// Chunk pricing configuration.
    #[command(flatten)]
    pub pricing: PricingArgs,

    /// Local store configuration.
    #[command(flatten)]
    pub localstore: LocalStoreArgs,

    /// Storage incentive configuration.
    #[command(flatten)]
    pub storage_incentives: StorageIncentiveArgs,

    /// Identity configuration.
    #[command(flatten)]
    pub identity: IdentityArgs,

    /// Run the node on the mainnet.
    #[arg(long, conflicts_with_all = ["testnet", "swarmspec"])]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub mainnet: bool,

    /// Run the node on the testnet.
    #[arg(long, conflicts_with_all = ["mainnet", "swarmspec"])]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarmspec: Option<PathBuf>,
}

impl Default for SwarmArgs {
    fn default() -> Self {
        Self {
            node_type: NodeTypeArg::default(),
            network: NetworkArgs::default(),
            bandwidth: BandwidthArgs::default(),
            pricing: PricingArgs::default(),
            localstore: LocalStoreArgs::default(),
            storage_incentives: StorageIncentiveArgs::default(),
            identity: IdentityArgs::default(),
            mainnet: false,
            testnet: false,
            swarmspec: None,
        }
    }
}

impl SwarmArgs {
    /// Validate all argument combinations.
    pub fn validate(&self) -> Result<(), String> {
        self.bandwidth.validate()?;
        Ok(())
    }

    /// Returns true if mainnet is selected (explicitly or by default).
    pub fn is_mainnet(&self) -> bool {
        self.mainnet || (!self.testnet && self.swarmspec.is_none())
    }

    /// Returns true if testnet is explicitly selected.
    pub fn is_testnet(&self) -> bool {
        self.testnet
    }

    /// Returns true if a custom swarmspec is provided.
    pub fn is_custom_network(&self) -> bool {
        self.swarmspec.is_some()
    }
}
