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

    /// Get the resolved database file path.
    ///
    /// Returns `None` when no persistence has been configured, in which
    /// case consumers should use an in-memory database.
    fn db_path(&self) -> Option<&Path> {
        None
    }
}
