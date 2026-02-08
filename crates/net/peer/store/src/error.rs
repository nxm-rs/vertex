//! Peer store error types.

use std::path::PathBuf;

/// Errors from peer store operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to open {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to deserialize {path}: {reason}")]
    Deserialize { path: PathBuf, reason: String },

    #[error("failed to serialize {path}: {reason}")]
    Serialize { path: PathBuf, reason: String },

    #[error("storage error: {0}")]
    Storage(String),
}

impl StoreError {
    /// Get the path associated with this error, if any.
    pub fn path(&self) -> Option<&PathBuf> {
        match self {
            Self::CreateDir { path, .. }
            | Self::Open { path, .. }
            | Self::Read { path, .. }
            | Self::Write { path, .. }
            | Self::Deserialize { path, .. }
            | Self::Serialize { path, .. } => Some(path),
            Self::Storage(_) => None,
        }
    }
}
