//! Identity lifecycle management for Swarm nodes.
//!
//! This module handles loading, creating, and persisting node identity,
//! including the signing key and nonce used for overlay address derivation.

use alloy_primitives::B256;
use alloy_signer::k256::ecdsa::SigningKey;
use eyre::{Result, WrapErr};
use rand::RngCore;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::{debug, info, warn};

use crate::{
    config::NodeMode,
    identity::NodeIdentity,
    keystore::{FileKeystore, Keystore, MemoryKeystore},
};

/// Key name used for the swarm signing key.
const SWARM_KEY_NAME: &str = "swarm";

/// Filename for the nonce file.
const NONCE_FILENAME: &str = "nonce";

/// Configuration for identity management.
#[derive(Debug, Clone)]
pub struct IdentityConfig {
    /// Whether to use ephemeral identity (not persisted).
    pub ephemeral: bool,

    /// Whether the node participates in redistribution.
    pub redistribution: bool,

    /// Whether SWAP is enabled.
    pub swap_enabled: bool,

    /// Whether staking is enabled.
    pub staking: bool,
}

impl IdentityConfig {
    /// Check if persistent identity is required based on configuration.
    pub fn requires_persistent(&self) -> bool {
        self.redistribution || self.staking || self.swap_enabled
    }
}

/// Manages the node identity lifecycle.
///
/// Handles loading existing identity from disk or creating new ephemeral/persistent identity.
pub struct IdentityManager {
    keystore: Box<dyn Keystore>,
    state_path: PathBuf,
}

impl IdentityManager {
    /// Create a new identity manager with file-based keystore.
    pub fn with_file_keystore(keystore_path: impl AsRef<Path>, state_path: impl AsRef<Path>) -> Self {
        Self {
            keystore: Box::new(FileKeystore::new(keystore_path)),
            state_path: state_path.as_ref().to_path_buf(),
        }
    }

    /// Create a new identity manager with in-memory keystore (for ephemeral nodes).
    pub fn ephemeral(state_path: impl AsRef<Path>) -> Self {
        Self {
            keystore: Box::new(MemoryKeystore::new()),
            state_path: state_path.as_ref().to_path_buf(),
        }
    }

    /// Load or create identity based on node mode and configuration.
    ///
    /// For ephemeral nodes (light mode or explicit --ephemeral), creates a random identity.
    /// For persistent nodes, loads from keystore or creates new keys.
    pub fn load_or_create(
        &self,
        network_id: u64,
        mode: NodeMode,
        config: &IdentityConfig,
        password: &str,
    ) -> Result<NodeIdentity> {
        // Determine if we should use ephemeral identity
        let use_ephemeral = config.ephemeral || matches!(mode, NodeMode::Light);

        if use_ephemeral {
            if config.requires_persistent() && config.ephemeral {
                warn!("Using ephemeral identity with incentives enabled - identity will be lost on restart!");
            }
            return self.create_ephemeral_identity(network_id, mode);
        }

        // For persistent identity, load or create
        self.load_or_create_persistent(network_id, mode, password)
    }

    /// Create a random ephemeral identity.
    fn create_ephemeral_identity(&self, network_id: u64, mode: NodeMode) -> Result<NodeIdentity> {
        debug!("Creating ephemeral identity");
        let is_full_node = !matches!(mode, NodeMode::Light);
        NodeIdentity::random(network_id, is_full_node)
    }

    /// Load or create a persistent identity.
    fn load_or_create_persistent(
        &self,
        network_id: u64,
        mode: NodeMode,
        password: &str,
    ) -> Result<NodeIdentity> {
        let is_full_node = !matches!(mode, NodeMode::Light);

        // Load or create signing key
        let signing_key = if self.keystore.exists(SWARM_KEY_NAME) {
            info!("Loading existing signing key from keystore");
            self.keystore.load(SWARM_KEY_NAME, password)?
        } else {
            info!("Generating new signing key");
            let key = SigningKey::random(&mut rand::thread_rng());
            self.keystore.save(SWARM_KEY_NAME, &key, password)?;
            key
        };

        // Load or create nonce
        let nonce = self.load_or_create_nonce()?;

        Ok(NodeIdentity::from_key_and_nonce(
            network_id,
            signing_key,
            nonce,
            is_full_node,
        ))
    }

    /// Load nonce from state or create a new one.
    fn load_or_create_nonce(&self) -> Result<B256> {
        let nonce_path = self.state_path.join(NONCE_FILENAME);

        if nonce_path.exists() {
            debug!("Loading existing nonce from {:?}", nonce_path);
            let bytes = fs::read(&nonce_path)
                .wrap_err_with(|| format!("Failed to read nonce from {:?}", nonce_path))?;

            if bytes.len() != 32 {
                return Err(eyre::eyre!(
                    "Invalid nonce file: expected 32 bytes, got {}",
                    bytes.len()
                ));
            }

            Ok(B256::from_slice(&bytes))
        } else {
            info!("Generating new nonce");

            // Ensure state directory exists
            fs::create_dir_all(&self.state_path)
                .wrap_err_with(|| format!("Failed to create state directory {:?}", self.state_path))?;

            // Generate random nonce
            let mut nonce_bytes = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut nonce_bytes);

            // Write to file
            fs::write(&nonce_path, &nonce_bytes)
                .wrap_err_with(|| format!("Failed to write nonce to {:?}", nonce_path))?;

            // Set restrictive permissions on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&nonce_path)?.permissions();
                perms.set_mode(0o600);
                fs::set_permissions(&nonce_path, perms)?;
            }

            Ok(B256::from(nonce_bytes))
        }
    }
}

/// Resolve password from various sources.
///
/// Priority: CLI argument > password file > interactive prompt
pub fn resolve_password(
    password: Option<&str>,
    password_file: Option<&Path>,
) -> Result<String> {
    // Check direct password
    if let Some(pwd) = password {
        return Ok(pwd.to_string());
    }

    // Check password file
    if let Some(path) = password_file {
        let content = fs::read_to_string(path)
            .wrap_err_with(|| format!("Failed to read password file {:?}", path))?;
        return Ok(content.trim().to_string());
    }

    // Try interactive prompt if we're in a terminal
    if atty::is(atty::Stream::Stdin) {
        return rpassword::prompt_password("Enter keystore password: ")
            .wrap_err("Failed to read password from terminal");
    }

    Err(eyre::eyre!(
        "No password provided. Use --password, --password-file, or VERTEX_PASSWORD environment variable"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_ephemeral_identity() {
        let dir = tempdir().unwrap();
        let manager = IdentityManager::ephemeral(dir.path());

        let config = IdentityConfig {
            ephemeral: true,
            redistribution: false,
            swap_enabled: false,
            staking: false,
        };

        let identity1 = manager
            .load_or_create(1, NodeMode::Light, &config, "")
            .unwrap();
        let identity2 = manager
            .load_or_create(1, NodeMode::Light, &config, "")
            .unwrap();

        // Ephemeral identities should be different each time
        assert_ne!(identity1.nonce(), identity2.nonce());
    }

    #[test]
    fn test_persistent_identity() {
        let dir = tempdir().unwrap();
        let keystore_path = dir.path().join("keys");
        let state_path = dir.path().join("state");

        let config = IdentityConfig {
            ephemeral: false,
            redistribution: true,
            swap_enabled: false,
            staking: false,
        };

        // First run - creates new identity
        let manager1 = IdentityManager::with_file_keystore(&keystore_path, &state_path);
        let identity1 = manager1
            .load_or_create(1, NodeMode::Full, &config, "test-password")
            .unwrap();

        // Second run - loads existing identity
        let manager2 = IdentityManager::with_file_keystore(&keystore_path, &state_path);
        let identity2 = manager2
            .load_or_create(1, NodeMode::Full, &config, "test-password")
            .unwrap();

        // Should have same nonce
        assert_eq!(identity1.nonce(), identity2.nonce());
    }

    #[test]
    fn test_nonce_persistence() {
        let dir = tempdir().unwrap();
        let manager = IdentityManager::ephemeral(dir.path());

        let nonce1 = manager.load_or_create_nonce().unwrap();
        let nonce2 = manager.load_or_create_nonce().unwrap();

        // Same nonce should be returned after persistence
        assert_eq!(nonce1, nonce2);
    }
}
