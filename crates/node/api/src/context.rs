//! Node context providing infrastructure to protocols.
//!
//! The [`NodeContext`] struct provides everything a protocol needs from the
//! node infrastructure to build and run itself.

use std::path::PathBuf;
use vertex_tasks::{Shutdown, TaskExecutor};

/// Infrastructure context provided by the node to protocols.
///
/// Contains everything a protocol needs from the node infrastructure
/// to build and run itself. This is created by the node builder and
/// passed to [`Protocol::build`](crate::Protocol::build).
///
/// # Example
///
/// ```ignore
/// use vertex_node_api::{NodeContext, Protocol};
///
/// // NodeContext is created by the node builder
/// let ctx = NodeContext::new(executor, data_dir);
///
/// // Protocol builds using the context
/// let built = SwarmLightProtocol::build(config, &ctx).await?;
/// ```
#[derive(Debug, Clone)]
pub struct NodeContext {
    /// Task executor for spawning background tasks.
    executor: TaskExecutor,
    /// Base data directory for persistent storage.
    data_dir: PathBuf,
}

impl NodeContext {
    /// Create a new node context with the given executor and data directory.
    pub fn new(executor: TaskExecutor, data_dir: PathBuf) -> Self {
        Self { executor, data_dir }
    }

    /// Get the task executor for spawning background tasks.
    pub fn executor(&self) -> &TaskExecutor {
        &self.executor
    }

    /// Get the base data directory for persistent storage.
    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    /// Get a clone of the shutdown signal receiver.
    ///
    /// This can be used to listen for shutdown signals in spawned tasks.
    pub fn shutdown_signal(&self) -> Shutdown {
        self.executor.on_shutdown_signal().clone()
    }
}
