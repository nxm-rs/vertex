//! Aggregated Swarm protocol CLI arguments.
//!
//! Convert to [`ProtocolConfig`](crate::config::ProtocolConfig) via `TryFrom` for validated configuration.

use clap::Args;
use serde::{Deserialize, Serialize};

use vertex_swarm_bandwidth::BandwidthArgs;
use vertex_swarm_identity::IdentityArgs;
use vertex_swarm_localstore::LocalStoreArgs;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_redistribution::RedistributionArgs;

use super::{NetworkArgs, SwarmSpecArgs};

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
        match arg {
            NodeTypeArg::Bootnode => SwarmNodeType::Bootnode,
            NodeTypeArg::Client => SwarmNodeType::Client,
            NodeTypeArg::Storer => SwarmNodeType::Storer,
        }
    }
}

impl From<SwarmNodeType> for NodeTypeArg {
    fn from(node_type: SwarmNodeType) -> Self {
        match node_type {
            SwarmNodeType::Bootnode => NodeTypeArg::Bootnode,
            SwarmNodeType::Client => NodeTypeArg::Client,
            SwarmNodeType::Storer => NodeTypeArg::Storer,
        }
    }
}

/// Aggregated Swarm protocol CLI arguments.
///
/// This struct is for CLI parsing and serialization only.
/// Convert to `ProtocolConfig` for runtime use.
/// Base price is a network-wide constant defined in the SwarmSpec.
#[derive(Args, Clone)]
pub struct ProtocolArgs {
    /// Swarm network specification and node mode.
    #[command(flatten)]
    pub spec: SwarmSpecArgs,

    #[command(flatten)]
    pub identity: IdentityArgs,

    #[command(flatten)]
    pub network: NetworkArgs,

    /// Bandwidth accounting configuration.
    #[command(flatten)]
    pub bandwidth: BandwidthArgs,

    #[command(flatten)]
    pub localstore: LocalStoreArgs,

    #[command(flatten)]
    pub redistribution: RedistributionArgs,
}
