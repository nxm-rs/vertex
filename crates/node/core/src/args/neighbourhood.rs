use std::str::FromStr;

use alloy_primitives::B256;
use clap::Args;

const DEFAULT_NONCE: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

/// Parameters for configuring the neighbourhood
#[derive(Debug, Clone, Args, PartialEq, Eq)]
#[command(next_help_heading = "Identity Configuration")]
pub struct NeighbourhoodArgs {
    /// Whether the node is to advertise itself as a full node.
    #[arg(long, value_name = "BOOL", default_value_t = false)]
    pub full: bool,

    /// The `nonce` for determining the node address.
    #[arg(long, value_name = "NONCE", value_parser = B256::from_str, default_value = DEFAULT_NONCE)]
    pub nonce: B256,
}
