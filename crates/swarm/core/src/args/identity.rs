//! Identity and keystore CLI arguments.

use alloy_primitives::B256;
use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vertex_swarm_api::SwarmIdentityConfig;

/// Identity and keystore configuration.
#[derive(Debug, Args, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
#[command(next_help_heading = "Identity")]
pub struct IdentityArgs {
    /// Password for keystore encryption/decryption.
    #[arg(long, env = "VERTEX_PASSWORD")]
    #[serde(skip)]
    pub password: Option<String>,

    /// Path to file containing keystore password.
    #[arg(long = "password-file")]
    #[serde(skip)]
    pub password_file: Option<PathBuf>,

    /// Nonce for overlay address derivation (hex-encoded, 32 bytes).
    ///
    /// The overlay address is derived as: keccak256(eth_address || network_id || nonce).
    /// Changing the nonce changes the node's position in the DHT.
    /// If not set, uses nonce from config file or generates a random one.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<B256>,

    /// Use ephemeral identity (random key, not persisted).
    ///
    /// Ephemeral nodes lose their overlay address on restart.
    #[arg(long)]
    pub ephemeral: bool,
}

impl SwarmIdentityConfig for IdentityArgs {
    fn ephemeral(&self) -> bool {
        self.ephemeral
    }
}
