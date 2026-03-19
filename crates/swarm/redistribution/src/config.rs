//! Validated storage incentives configuration.

use vertex_swarm_api::SwarmStorageConfig;

/// Validated storage incentives configuration.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    redistribution_enabled: bool,
}

impl StorageConfig {
    /// Create a new storage configuration.
    pub fn new(redistribution_enabled: bool) -> Self {
        Self {
            redistribution_enabled,
        }
    }

    /// Whether redistribution is enabled.
    pub fn redistribution_enabled(&self) -> bool {
        self.redistribution_enabled
    }
}

impl SwarmStorageConfig for StorageConfig {
    fn redistribution_enabled(&self) -> bool {
        self.redistribution_enabled
    }
}
