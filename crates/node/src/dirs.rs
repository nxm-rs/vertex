//! Directory management for the Vertex Swarm node.

use crate::cli::DataDirArgs;
use directories::ProjectDirs;
use eyre::{eyre, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};
use vertex_swarmspec::NetworkSpec;

/// Returns the default project directories for Vertex Swarm.
pub fn default_project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("org", "vertex", "vertex")
}

/// Returns the default data directory path.
pub fn default_data_dir() -> Option<PathBuf> {
    default_project_dirs().map(|dirs| dirs.data_dir().to_path_buf())
}

/// Returns the default config file path.
pub fn default_config_file() -> Option<PathBuf> {
    default_project_dirs().map(|dirs| dirs.config_dir().join("vertex.toml"))
}

/// Returns the default logs directory path.
pub fn default_logs_dir() -> Option<PathBuf> {
    default_project_dirs().map(|dirs| dirs.cache_dir().join("logs"))
}

/// A helper for managing data directories for a specific network.
pub struct DataDirs {
    /// Root data directory
    pub root: PathBuf,
    /// Network-specific subdirectory
    pub network: PathBuf,
    /// Database directory
    pub db: PathBuf,
    /// Static files directory
    pub static_files: PathBuf,
    /// Keystore directory
    pub keystore: PathBuf,
    /// P2P identity secret key path
    pub p2p_secret: PathBuf,
    /// JWT secret file path
    pub jwt_secret: PathBuf,
    /// Logs directory
    pub logs: PathBuf,
}

impl DataDirs {
    /// Create a new `DataDirs` instance for the given network specification and command line args.
    pub fn new(network: &NetworkSpec, args: &DataDirArgs) -> Result<Self> {
        let root = args
            .datadir
            .clone()
            .unwrap_or_else(|| default_data_dir().unwrap_or_else(|| PathBuf::from(".vertex")));

        let network_dir = root.join(network.network_name());
        let db_dir = network_dir.join("db");
        let static_files_dir = args
            .static_files_path
            .clone()
            .unwrap_or_else(|| network_dir.join("static_files"));
        let keystore_dir = network_dir.join("keystore");
        let p2p_secret_path = network_dir.join("p2p-secret.key");
        let jwt_secret_path = args
            .auth_jwtsecret
            .clone()
            .unwrap_or_else(|| network_dir.join("jwt.hex"));
        let logs_dir = root.join("logs");

        // Ensure directories exist
        for dir in [
            &network_dir,
            &db_dir,
            &static_files_dir,
            &keystore_dir,
            &logs_dir,
        ] {
            fs::create_dir_all(dir)
                .map_err(|e| eyre!("Failed to create directory {}: {}", dir.display(), e))?;
        }

        Ok(Self {
            root,
            network: network_dir,
            db: db_dir,
            static_files: static_files_dir,
            keystore: keystore_dir,
            p2p_secret: p2p_secret_path,
            jwt_secret: jwt_secret_path,
            logs: logs_dir,
        })
    }

    /// Returns the path to the storage directory for chunks.
    pub fn chunks_dir(&self) -> PathBuf {
        self.network.join("chunks")
    }

    /// Returns the path to the bandwidth accounting state.
    pub fn bandwidth_dir(&self) -> PathBuf {
        self.network.join("bandwidth")
    }

    /// Returns the path to the config file.
    pub fn config_file(&self) -> PathBuf {
        self.network.join("config.toml")
    }

    /// Returns the path to store transaction receipts for processing payments.
    pub fn receipts_dir(&self) -> PathBuf {
        self.network.join("receipts")
    }

    /// Returns the path to the peer storage file.
    pub fn peers_file(&self) -> PathBuf {
        self.network.join("peers.json")
    }
}

/// Parse a path with environment variable expansion and tilde expansion.
pub fn parse_path(path: &str) -> Result<PathBuf> {
    let expanded = shellexpand::full(path)?;
    Ok(PathBuf::from(expanded.into_owned()))
}
