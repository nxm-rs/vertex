//! Identity and keystore CLI arguments.

use alloy_primitives::B256;
use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vertex_swarm_api::SwarmIdentityConfig;

/// Identity and keystore configuration.
///
/// By default, a persistent identity is created with:
/// - Keystore at `{data-dir}/keystore/`
/// - Auto-generated password saved to `{data-dir}/password` (mode 0600)
///
/// Use `--ephemeral` for testing to skip keystore creation entirely.
#[derive(Debug, Args, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
#[command(next_help_heading = "Identity")]
pub struct IdentityArgs {
    /// Use ephemeral identity (random key, not persisted).
    #[arg(long, conflicts_with_all = ["password", "password_file", "keystore_dir", "nonce"])]
    pub ephemeral: bool,

    /// Keystore directory path.
    #[arg(long = "keystore-dir", value_name = "PATH")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keystore_dir: Option<PathBuf>,

    /// Keystore password.
    #[arg(long, conflicts_with = "password_file")]
    #[serde(skip)]
    pub password: Option<String>,

    /// Path to keystore password file.
    #[arg(long = "password-file", value_name = "PATH")]
    #[serde(skip)]
    pub password_file: Option<PathBuf>,

    /// Nonce for overlay address derivation (32 bytes, hex).
    #[arg(long, value_name = "HEX")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<B256>,
}

impl SwarmIdentityConfig for IdentityArgs {
    fn ephemeral(&self) -> bool {
        self.ephemeral
    }
}
