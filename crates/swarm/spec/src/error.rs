//! Error types for swarmspec operations.

/// Error type for SwarmSpec file operations.
#[cfg(feature = "std")]
#[derive(Debug, thiserror::Error)]
pub enum SwarmSpecFileError {
    /// IO error reading/writing file.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON parsing/serialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
