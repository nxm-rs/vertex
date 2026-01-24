//! Builder context providing runtime state for component builders.

use std::sync::Arc;

use vertex_swarm_api::{LightTypes, NetworkConfig};
use vertex_tasks::TaskExecutor;

/// Runtime context passed to component builders.
///
/// Contains everything needed to construct Swarm components:
/// - Identity and spec
/// - Network configuration
/// - Task executor for spawning
pub struct SwarmBuilderContext<'cfg, Types: LightTypes, Cfg: NetworkConfig> {
    /// The node's cryptographic identity.
    pub identity: Arc<Types::Identity>,

    /// Network configuration.
    pub config: &'cfg Cfg,

    /// Task executor for spawning background tasks.
    pub executor: TaskExecutor,
}

impl<'cfg, Types: LightTypes, Cfg: NetworkConfig> SwarmBuilderContext<'cfg, Types, Cfg> {
    /// Create a new builder context.
    pub fn new(identity: Arc<Types::Identity>, config: &'cfg Cfg, executor: TaskExecutor) -> Self {
        Self {
            identity,
            config,
            executor,
        }
    }
}
