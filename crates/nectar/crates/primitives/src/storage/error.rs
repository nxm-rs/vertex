//! Errors specific to storage operations

use thiserror::Error;

/// Result type for storage operations
pub type Result<T> = std::result::Result<T, StorageError>;

/// Errors specific to storage operations
#[derive(Error, Debug)]
pub enum StorageError {
    /// Storage authentication failed
    #[error("Storage authentication failed: {0}")]
    AuthenticationFailed(String),

    /// Storage authorization failed
    #[error("Storage authorization failed: {0}")]
    AuthorizationFailed(String),

    /// Invalid storage credential
    #[error("Invalid storage credential: {0}")]
    InvalidCredential(String),

    /// Expired credential
    #[error("Expired storage credential")]
    ExpiredCredential,

    /// Used credential
    #[error("Storage credential already used")]
    UsedCredential,

    /// Invalid batch ID
    #[error("Invalid batch ID: {0}")]
    InvalidBatchId(String),

    /// Insufficient storage depth
    #[error("Insufficient storage depth: required {required}, got {available}")]
    InsufficientDepth {
        /// Required depth
        required: u8,
        /// Available depth
        available: u8,
    },

    /// Insufficient storage capacity
    #[error("Insufficient storage capacity: required {required}, available {available}")]
    InsufficientCapacity {
        /// Required capacity
        required: u64,
        /// Available capacity
        available: u64,
    },
}

impl StorageError {
    /// Create a new authentication failed error
    pub fn authentication_failed<S: Into<String>>(msg: S) -> Self {
        Self::AuthenticationFailed(msg.into())
    }

    /// Create a new authorization failed error
    pub fn authorization_failed<S: Into<String>>(msg: S) -> Self {
        Self::AuthorizationFailed(msg.into())
    }

    /// Create a new invalid credential error
    pub fn invalid_credential<S: Into<String>>(msg: S) -> Self {
        Self::InvalidCredential(msg.into())
    }

    /// Create a new invalid batch ID error
    pub fn invalid_batch_id<S: Into<String>>(msg: S) -> Self {
        Self::InvalidBatchId(msg.into())
    }

    /// Create a new insufficient depth error
    pub fn insufficient_depth(required: u8, available: u8) -> Self {
        Self::InsufficientDepth {
            required,
            available,
        }
    }

    /// Create a new insufficient capacity error
    pub fn insufficient_capacity(required: u64, available: u64) -> Self {
        Self::InsufficientCapacity {
            required,
            available,
        }
    }
}
