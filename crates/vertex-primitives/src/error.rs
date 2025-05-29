//! Error types for the Vertex Swarm node

use alloc::string::String;

/// Common error type for all Vertex operations
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Error related to chunk operations
    #[error("Chunk error: {0}")]
    Chunk(String),

    /// Error related to storage operations
    #[error("Storage error: {0}")]
    Storage(String),

    /// Error related to network operations
    #[error("Network error: {0}")]
    Network(String),

    /// Error related to authentication
    #[error("Authentication error: {0}")]
    Authentication(String),

    /// Error related to authorization
    #[error("Authorization error: {0}")]
    Authorization(String),

    /// Error related to accounting
    #[error("Accounting error: {0}")]
    Accounting(String),

    /// Error when a resource is not found
    #[error("Not found: {0}")]
    NotFound(String),

    /// Error when a resource already exists
    #[error("Already exists: {0}")]
    AlreadyExists(String),

    /// Error when an operation is invalid
    #[error("Invalid operation: {0}")]
    InvalidOperation(String),

    /// Error when an operation times out
    #[error("Timeout: {0}")]
    Timeout(String),

    /// Error related to IO operations
    #[error("IO error: {0}")]
    Io(String),

    /// Error related to configuration
    #[error("Configuration error: {0}")]
    Configuration(String),

    /// Error related to blockchain operations
    #[error("Blockchain error: {0}")]
    Blockchain(String),

    /// Other errors
    #[error("Other error: {0}")]
    Other(String),
}

impl Error {
    /// Creates a new chunk error
    pub fn chunk(msg: impl Into<String>) -> Self {
        Self::Chunk(msg.into())
    }

    /// Creates a new storage error
    pub fn storage(msg: impl Into<String>) -> Self {
        Self::Storage(msg.into())
    }

    /// Creates a new network error
    pub fn network(msg: impl Into<String>) -> Self {
        Self::Network(msg.into())
    }

    /// Creates a new authentication error
    pub fn authentication(msg: impl Into<String>) -> Self {
        Self::Authentication(msg.into())
    }

    /// Creates a new authorization error
    pub fn authorization(msg: impl Into<String>) -> Self {
        Self::Authorization(msg.into())
    }

    /// Creates a new accounting error
    pub fn accounting(msg: impl Into<String>) -> Self {
        Self::Accounting(msg.into())
    }

    /// Creates a new not found error
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    /// Creates a new already exists error
    pub fn already_exists(msg: impl Into<String>) -> Self {
        Self::AlreadyExists(msg.into())
    }

    /// Creates a new invalid operation error
    pub fn invalid_operation(msg: impl Into<String>) -> Self {
        Self::InvalidOperation(msg.into())
    }

    /// Creates a new timeout error
    pub fn timeout(msg: impl Into<String>) -> Self {
        Self::Timeout(msg.into())
    }

    /// Creates a new IO error
    pub fn io(msg: impl Into<String>) -> Self {
        Self::Io(msg.into())
    }

    /// Creates a new other error
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}
