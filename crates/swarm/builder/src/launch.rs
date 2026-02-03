//! Swarm launch context for node initialization.
//!
//! This module provides the Swarm-specific launch context that contains
//! all the components needed to launch a Swarm node.

use std::path::PathBuf;
use std::sync::Arc;

use eyre::{Result, WrapErr};
use vertex_node_builder::LaunchContext;
use vertex_node_core::config::FullNodeConfig;
use vertex_node_core::dirs::DataDirs;
use vertex_swarm_identity::Identity;
use vertex_swarm_node::ProtocolConfig;
use vertex_swarm_peermanager::PeerStore;
use vertex_swarmspec::Hive;
use vertex_tasks::TaskExecutor;

/// Swarm-specific launch context.
///
/// Contains all the components needed to launch a Swarm node:
/// - Generic infrastructure context (executor, data dirs)
/// - Network specification (mainnet/testnet)
/// - Node identity (signing key, overlay address)
/// - Peer store for persistence
/// - Swarm protocol configuration
///
/// Create this using [`SwarmLaunchContext::from_args`] in `vertex-swarm-node`,
/// or construct it directly for programmatic use.
pub struct SwarmLaunchContext {
    /// Generic infrastructure context.
    pub base: LaunchContext,
    /// Loaded and merged configuration.
    pub config: FullNodeConfig<ProtocolConfig>,
    /// Network specification.
    pub spec: Arc<Hive>,
    /// Node identity.
    pub identity: Identity,
    /// Peer store for persistence.
    pub peer_store: Arc<PeerStore>,
    /// Path to peers database file.
    pub peers_path: PathBuf,
}

impl SwarmLaunchContext {
    /// Create a new launch context with all components.
    pub fn new(
        base: LaunchContext,
        config: FullNodeConfig<ProtocolConfig>,
        spec: Arc<Hive>,
        identity: Identity,
        peer_store: Arc<PeerStore>,
        peers_path: PathBuf,
    ) -> Self {
        Self {
            base,
            config,
            spec,
            identity,
            peer_store,
            peers_path,
        }
    }

    /// Get the data directory root.
    pub fn data_dir(&self) -> &PathBuf {
        self.base.data_dir()
    }

    /// Get the task executor.
    pub fn executor(&self) -> &TaskExecutor {
        &self.base.executor
    }

    /// Get the data directories.
    pub fn dirs(&self) -> &DataDirs {
        &self.base.dirs
    }

    /// Get the gRPC address from config.
    pub fn grpc_addr(&self) -> std::net::SocketAddr {
        use std::net::{IpAddr, SocketAddr};
        SocketAddr::new(
            self.config
                .infra
                .api
                .grpc_addr
                .parse()
                .unwrap_or(IpAddr::from([127, 0, 0, 1])),
            self.config.infra.api.grpc_port,
        )
    }
}

/// Resolve password from argument or file.
pub fn resolve_password(password: Option<&str>, password_file: Option<&str>) -> Result<String> {
    use std::fs;

    if let Some(pw) = password {
        return Ok(pw.to_string());
    }

    if let Some(path) = password_file {
        let password = fs::read_to_string(path)
            .wrap_err_with(|| format!("Failed to read password file: {}", path))?
            .trim()
            .to_string();
        return Ok(password);
    }

    // Default empty password (ephemeral nodes)
    Ok(String::new())
}

/// Load a signer from an Ethereum keystore file.
pub fn load_signer_from_keystore(
    keystore_path: &std::path::Path,
    password: &str,
) -> Result<alloy_signer_local::PrivateKeySigner> {
    use alloy_signer_local::PrivateKeySigner;
    use tracing::info;

    info!(
        "Loading signing key from keystore: {}",
        keystore_path.display()
    );
    PrivateKeySigner::decrypt_keystore(keystore_path, password)
        .wrap_err_with(|| format!("Failed to decrypt keystore at {}", keystore_path.display()))
}

/// Create a new random signer and save it to a keystore.
pub fn create_and_save_signer(
    keystore_path: &std::path::Path,
    password: &str,
) -> Result<alloy_signer_local::PrivateKeySigner> {
    use alloy_signer_local::PrivateKeySigner;
    use std::fs;
    use std::path::Path;
    use tracing::info;

    info!("Generating new signing key");

    if let Some(parent) = keystore_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let name = keystore_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("swarm");

    let dir = keystore_path.parent().unwrap_or(Path::new("."));

    // Use rand 0.8 for alloy keystore compatibility
    let (signer, _uuid) =
        PrivateKeySigner::new_keystore(dir, &mut rand_08::thread_rng(), password, Some(name))
            .wrap_err("Failed to create keystore")?;

    #[cfg(unix)]
    if keystore_path.exists() {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(keystore_path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(keystore_path, perms)?;
    }

    Ok(signer)
}
