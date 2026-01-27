//! Light node preset.

/// Light Swarm node preset.
///
/// Light nodes can retrieve chunks but cannot store or upload them.
/// This is the simplest node type, suitable for read-only access.
pub struct SwarmLightNode<Cfg> {
    config: Cfg,
}

impl<Cfg> SwarmLightNode<Cfg> {
    /// Create a new light node with the given configuration.
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
