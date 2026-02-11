//! Infrastructure context trait for protocol launching.

use std::path::Path;
use vertex_tasks::TaskExecutor;

/// Infrastructure context providing access to shared node infrastructure.
///
/// This trait is implemented by launch contexts and provides protocols with
/// access to the task executor and data directory during building.
pub trait InfrastructureContext: Send + Sync {
    /// Get the task executor for spawning background tasks.
    fn executor(&self) -> &TaskExecutor;

    /// Get the data directory for persistent storage.
    fn data_dir(&self) -> &Path;
}
