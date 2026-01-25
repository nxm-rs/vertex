//! Identity and keystore CLI arguments.

use alloy_primitives::B256;
use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vertex_swarm_api::IdentityConfig;

/// Identity and keystore configuration.
#[derive(Debug, Args, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
#[command(next_help_heading = "Identity")]
pub struct IdentityArgs {
    /// Password for keystore encryption/decryption.
    ///
    /// Can also be set via the VERTEX_PASSWORD environment variable.
    #[arg(long, env = "VERTEX_PASSWORD")]
    #[serde(skip)] // Never persist passwords
    pub password: Option<String>,

    /// Path to file containing keystore password.
    #[arg(long = "password-file")]
    #[serde(skip)] // Never persist password file paths
    pub password_file: Option<PathBuf>,

    /// Nonce for overlay address derivation (hex-encoded, 32 bytes).
    ///
    /// The overlay address is derived as: keccak256(eth_address || network_id || nonce).
    /// Changing the nonce changes the node's position in the DHT.
    /// If not set, uses nonce from config file or generates a random one.
    #[arg(long, value_parser = parse_nonce)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<B256>,

    /// Use ephemeral identity (random key, not persisted).
    ///
    /// Ephemeral nodes lose their overlay address on restart.
    #[arg(long)]
    pub ephemeral: bool,
}

/// Parse a hex-encoded 32-byte nonce from CLI.
fn parse_nonce(s: &str) -> Result<B256, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).map_err(|e| format!("invalid hex: {}", e))?;
    if bytes.len() != 32 {
        return Err(format!("nonce must be 32 bytes, got {}", bytes.len()));
    }
    Ok(B256::from_slice(&bytes))
}

impl IdentityConfig for IdentityArgs {
    fn ephemeral(&self) -> bool {
        self.ephemeral
    }

    fn requires_persistent(&self) -> bool {
        // IdentityArgs alone cannot determine this - it depends on node type.
        // This returns a conservative default; the node command should check
        // node type to determine actual persistence requirements.
        !self.ephemeral
    }
}
