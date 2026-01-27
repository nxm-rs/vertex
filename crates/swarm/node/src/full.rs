//! Full node preset.

/// Full Swarm node preset.
///
/// Full nodes store chunks locally and sync with neighbors.
/// This is the most complete node type, suitable for running
/// infrastructure and earning rewards.
pub struct SwarmFullNode<Cfg> {
    config: Cfg,
}

impl<Cfg> SwarmFullNode<Cfg> {
    /// Create a new full node with the given configuration.
    pub fn new(config: Cfg) -> Self {
        Self { config }
    }

    /// Get the configuration.
    pub fn config(&self) -> &Cfg {
        &self.config
    }

    /// Consume and return the configuration.
    pub fn into_config(self) -> Cfg {
        self.config
    }
}
