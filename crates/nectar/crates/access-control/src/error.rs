//! Error types for access control operations.

use thiserror::Error;

/// Generic result type for operations in this crate
pub type Result<T> = std::result::Result<T, Error>;

/// Primary error type for the access control crate
#[derive(Error, Debug)]
pub enum Error {
    /// Authentication errors
    #[error("Authentication error: {0}")]
    Authentication(String),

    /// Authorization errors
    #[error("Authorization error: {0}")]
    Authorization(String),

    /// Accounting errors
    #[error("Accounting error: {0}")]
    Accounting(String),

    /// Credential errors
    #[error("Credential error: {0}")]
    Credential(String),

    /// Resource errors
    #[error("Resource error: {0}")]
    Resource(String),

    /// Expired credential
    #[error("Expired credential")]
    ExpiredCredential,

    /// Credential already used
    #[error("Credential already used")]
    CredentialAlreadyUsed,

    /// Insufficient resources
    #[error("Insufficient resources: {required} required, {available} available")]
    InsufficientResources {
        /// Required amount
        required: u64,
        /// Available amount
        available: u64,
    },

    /// Reservation errors
    #[error("Reservation error: {0}")]
    Reservation(String),

    /// I/O errors
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Other errors
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Create a new authentication error
    pub fn authentication<S: Into<String>>(msg: S) -> Self {
        Self::Authentication(msg.into())
    }

    /// Create a new authorization error
    pub fn authorization<S: Into<String>>(msg: S) -> Self {
        Self::Authorization(msg.into())
    }

    /// Create a new accounting error
    pub fn accounting<S: Into<String>>(msg: S) -> Self {
        Self::Accounting(msg.into())
    }

    /// Create a new credential error
    pub fn credential<S: Into<String>>(msg: S) -> Self {
        Self::Credential(msg.into())
    }

    /// Create a new resource error
    pub fn resource<S: Into<String>>(msg: S) -> Self {
        Self::Resource(msg.into())
    }

    /// Create a new reservation error
    pub fn reservation<S: Into<String>>(msg: S) -> Self {
        Self::Reservation(msg.into())
    }

    /// Create a generic error
    pub fn other<S: Into<String>>(msg: S) -> Self {
        Self::Other(msg.into())
    }
}
