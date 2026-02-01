//! Aggregated Swarm protocol CLI arguments.

use std::path::PathBuf;

use clap::Args;
use serde::{Deserialize, Serialize};

use vertex_swarm_bandwidth::BandwidthArgs;
use vertex_swarm_bandwidth_pricing::PricingArgs;
use vertex_swarm_identity::IdentityArgs;
use vertex_swarm_localstore::LocalStoreArgs;
use vertex_swarm_primitives::SwarmNodeType;

use super::{NetworkArgs, StorageIncentiveArgs};

/// CLI argument for node mode selection. Maps to [`SwarmNodeType`].
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    clap::ValueEnum,
    strum::FromRepr,
    Serialize,
    Deserialize,
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

/// Aggregated Swarm protocol CLI arguments.
///
/// Combines all Swarm-specific configuration into a single flattened group
/// for use with clap.
#[derive(Debug, Args, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SwarmArgs {
    /// Node mode: bootnode, client, or storer.
    #[arg(long = "mode", value_enum, default_value_t = NodeTypeArg::Client)]
    pub node_type: NodeTypeArg,

    #[command(flatten)]
    pub network: NetworkArgs,

    #[command(flatten)]
    pub bandwidth: BandwidthArgs,

    #[command(flatten)]
    pub pricing: PricingArgs,

    #[command(flatten)]
    pub localstore: LocalStoreArgs,

    #[command(flatten)]
    pub storage_incentives: StorageIncentiveArgs,

    #[command(flatten)]
    pub identity: IdentityArgs,

    /// Run on mainnet.
    #[arg(long, conflicts_with_all = ["testnet", "swarmspec"])]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub mainnet: bool,

    /// Run on testnet.
    #[arg(long, conflicts_with_all = ["mainnet", "swarmspec"])]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub testnet: bool,

    /// Path to custom SwarmSpec file for local/dev networks.
    #[arg(long, conflicts_with_all = ["mainnet", "testnet"], value_name = "PATH")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarmspec: Option<PathBuf>,
}

impl SwarmArgs {
    /// Validate argument combinations.
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
