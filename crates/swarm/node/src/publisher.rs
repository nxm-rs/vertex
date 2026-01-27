//! Publisher node preset.

/// Publisher Swarm node preset.
///
/// Publisher nodes can retrieve and upload chunks but don't store them locally.
/// This is suitable for applications that need to publish content to Swarm.
pub struct SwarmPublisherNode<Cfg> {
    config: Cfg,
}

impl<Cfg> SwarmPublisherNode<Cfg> {
    /// Create a new publisher node with the given configuration.
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
