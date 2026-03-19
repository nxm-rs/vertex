//! Identity and keystore CLI arguments.

use crate::Identity;
use crate::keystore::{create_and_save_signer, load_signer_from_keystore, resolve_password};
use alloy_primitives::B256;
use clap::Args;
use eyre::Result;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::debug;
use vertex_swarm_api::SwarmIdentityConfig;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_spec::Spec;

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

impl IdentityArgs {
    /// Create an Identity from these CLI arguments.
    pub fn identity(
        &self,
        spec: Arc<Spec>,
        network_dir: &Path,
        node_type: SwarmNodeType,
    ) -> Result<Arc<Identity>> {
        let use_ephemeral = self.ephemeral || !node_type.requires_persistent_identity();

        if use_ephemeral {
            return Ok(Arc::new(Identity::random(spec, node_type)));
        }

        // Persistent identity
        let keystore_path = self
            .keystore_dir
            .clone()
            .unwrap_or_else(|| network_dir.join("keystore").join("swarm"));

        let password = resolve_password(
            self.password.as_deref(),
            self.password_file
                .as_ref()
                .map(|p| p.to_string_lossy())
                .as_deref(),
        )?;

        let signer = if keystore_path.exists() {
            load_signer_from_keystore(&keystore_path, &password)?
        } else {
            create_and_save_signer(&keystore_path, &password)?
        };

        let nonce = self.nonce.unwrap_or_else(|| {
            let mut bytes = [0u8; 32];
            rand::rng().fill(&mut bytes);
            debug!("Generated new nonce");
            B256::from(bytes)
        });

        Ok(Arc::new(Identity::new(signer, nonce, spec, node_type)))
    }
}

impl SwarmIdentityConfig for IdentityArgs {
    fn ephemeral(&self) -> bool {
        self.ephemeral
    }
}
