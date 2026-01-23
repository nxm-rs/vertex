//! Directory management for the Vertex Swarm node.

use crate::cli::DataDirArgs;
use directories::ProjectDirs;
use eyre::{eyre, Result};
use std::{fs, path::PathBuf, sync::Arc};
use vertex_swarmspec::Hive;

/// Returns the default project directories for Vertex Swarm.
pub fn default_project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("org", "vertex", "vertex")
}

/// Returns the default data directory path.
pub fn default_data_dir() -> Option<PathBuf> {
    default_project_dirs().map(|dirs| dirs.data_dir().to_path_buf())
}

/// A helper for managing data directories for a specific network.
pub struct DataDirs {
    /// Root data directory
    pub root: PathBuf,
    /// Network-specific subdirectory
    pub network: PathBuf,
}

impl DataDirs {
    /// Create a new `DataDirs` instance for the given network specification and command line args.
    pub fn new(spec: &Arc<Hive>, args: &DataDirArgs) -> Result<Self> {
        let root = args
            .datadir
            .clone()
            .unwrap_or_else(|| default_data_dir().unwrap_or_else(|| PathBuf::from(".vertex")));

        let network_dir = root.join(&spec.network_name);

        // Ensure network directory exists
        fs::create_dir_all(&network_dir).map_err(|e| {
            eyre!(
                "Failed to create directory {}: {}",
                network_dir.display(),
                e
            )
        })?;

        Ok(Self {
            root,
            network: network_dir,
        })
    }

    /// Returns the path to the config file.
    pub fn config_file(&self) -> PathBuf {
        self.network.join("config.toml")
    }

    /// Returns the path to the state directory for persistent node state.
    ///
    /// This includes the nonce file and other identity-related state.
    pub fn state_dir(&self) -> PathBuf {
        self.network.join("state")
    }

    /// Returns the path to the keystore directory.
    pub fn keys_dir(&self) -> PathBuf {
        self.network.join("keystore")
    }

    /// Returns the path to the peers database file.
    pub fn peers_file(&self) -> PathBuf {
        self.state_dir().join("peers.json")
    }
}
