//! Errors specific to BMT operations

use thiserror::Error;

/// Result type for BMT operations
pub type Result<T> = std::result::Result<T, DigestError>;

/// Errors specific to digest operations
#[derive(Error, Debug)]
pub enum DigestError {
    /// Invalid input size
    #[error("Invalid input size: {0}")]
    InvalidInputSize(String),

    /// Digest computation failed
    #[error("Digest computation failed: {0}")]
    ComputationFailed(String),

    /// Verification failed
    #[error("Verification failed: {0}")]
    VerificationFailed(String),

    /// Invalid proof
    #[error("Invalid proof: {0}")]
    InvalidProof(String),
}

impl DigestError {
    /// Create a new invalid input size error
    pub fn invalid_input_size<S: Into<String>>(msg: S) -> Self {
        Self::InvalidInputSize(msg.into())
    }

    /// Create a new computation failed error
    pub fn computation_failed<S: Into<String>>(msg: S) -> Self {
        Self::ComputationFailed(msg.into())
    }

    /// Create a new verification failed error
    pub fn verification_failed<S: Into<String>>(msg: S) -> Self {
        Self::VerificationFailed(msg.into())
    }

    /// Create a new invalid proof error
    pub fn invalid_proof<S: Into<String>>(msg: S) -> Self {
        Self::InvalidProof(msg.into())
    }
}
