//! Identity and keystore CLI arguments.

use crate::Identity;
use crate::keystore::{create_and_save_signer, load_signer_from_keystore, resolve_password};
use alloy_primitives::B256;
use clap::Args;
use eyre::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::debug;
use vertex_swarm_api::{IdentityError, SwarmIdentityConfig};
use vertex_swarm_primitives::{Nonce, SwarmNodeType};
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
        // Bootnodes and storers must run with a persistent (keystore-backed)
        // identity - their overlay address is part of the network contract.
        if self.ephemeral && node_type.requires_persistent_identity() {
            return Err(IdentityError::EphemeralWhenPersistent { node_type }.into());
        }

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

        let nonce = self.nonce.map(Nonce::from).unwrap_or_else(|| {
            debug!("Generated new nonce");
            crate::random_nonce()
        });

        Ok(Arc::new(Identity::new(signer, nonce, spec, node_type)))
    }
}

impl SwarmIdentityConfig for IdentityArgs {
    fn ephemeral(&self) -> bool {
        self.ephemeral
    }
}
