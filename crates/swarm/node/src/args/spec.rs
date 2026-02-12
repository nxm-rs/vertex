//! Swarm specification CLI arguments.

use std::sync::Arc;

use clap::Args;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_spec::{DefaultSpecParser, Spec};

use super::swarm::NodeTypeArg;

/// Swarm specification and node mode configuration.
///
/// This controls which Swarm network to connect to and what mode the node
/// operates in (bootnode, client, or storer).
#[derive(Args, Clone)]
#[command(next_help_heading = "Swarm Specification")]
pub struct SwarmSpecArgs {
    /// Swarm network: "mainnet", "testnet", "dev", or path to spec file.
    #[arg(long, default_value = "mainnet", value_parser = DefaultSpecParser::parser())]
    pub swarm: Arc<Spec>,

    /// Node mode: bootnode, client, or storer.
    #[arg(long = "mode", value_enum, default_value_t = NodeTypeArg::Client)]
    pub node_type: NodeTypeArg,
}

impl SwarmSpecArgs {
    /// Get the parsed Swarm specification.
    pub fn spec(&self) -> &Arc<Spec> {
        &self.swarm
    }

    /// Get the node type as [`SwarmNodeType`].
    pub fn node_type(&self) -> SwarmNodeType {
        self.node_type.into()
    }
}
