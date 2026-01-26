//! Directory management for Vertex nodes.

use crate::args::DataDirArgs;
use crate::constants::DEFAULT_DATA_DIR_NAME;
use directories::ProjectDirs;
use eyre::{Result, eyre};
use std::{fs, path::PathBuf};

/// Returns the default project directories for Vertex.
pub fn default_project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("org", DEFAULT_DATA_DIR_NAME, DEFAULT_DATA_DIR_NAME)
}

/// Returns the default data directory path.
pub fn default_data_dir() -> Option<PathBuf> {
    default_project_dirs().map(|dirs| dirs.data_dir().to_path_buf())
}

/// A helper for managing data directories for a specific network.
#[derive(Debug, Clone)]
pub struct DataDirs {
    /// Root data directory
    pub root: PathBuf,
    /// Network-specific subdirectory
    pub network: PathBuf,
}

impl DataDirs {
    /// Create a new `DataDirs` instance for the given network name and command line args.
    ///
    /// The `network_name` is used to create a network-specific subdirectory.
    pub fn new(network_name: &str, args: &DataDirArgs) -> Result<Self> {
        let fallback_dir = format!(".{}", DEFAULT_DATA_DIR_NAME);
        let root = args
            .datadir
            .clone()
            .unwrap_or_else(|| default_data_dir().unwrap_or_else(|| PathBuf::from(&fallback_dir)));

        let network_dir = root.join(network_name);

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
}
