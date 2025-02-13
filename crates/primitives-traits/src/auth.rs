use alloy::primitives::{BlockTimestamp, SignatureError};
use bytes::Bytes;
use std::io;
use thiserror::Error;

use crate::Chunk;

/// Fundamental proof of authorization for a chunk
pub trait AuthProof: Send + Sync {
    fn proof_data(&self) -> Bytes;
}

/// Core authorization validation
pub trait Authorizer: Send + Sync {
    type Proof: AuthProof;

    /// Get total number of chunks held within storage by this authorizer
    fn authorized_chunk_count(&self) -> u64;

    /// Validate a proof for a chunk
    fn validate(&self, chunk: &impl Chunk, proof: &Self::Proof) -> AuthResult<()>;
}

/// Time-bound authorization capabilities
pub trait TimeBoundAuthorizer: Authorizer {
    fn cleanup_expired(&mut self, now: BlockTimestamp) -> AuthResult<u64>;
}

/// Capacity-tracked authorization
pub trait ResourceBoundAuthorizer: Authorizer {
    fn total_capacity(&self) -> u64;
    fn used_capacity(&self) -> u64;
    fn available_capacity(&self) -> u64 {
        self.total_capacity().saturating_sub(self.used_capacity())
    }
}

/// Authorization creation
pub trait AuthProofGenerator: Send + Sync {
    type Proof: AuthProof;

    /// Generate a proof for a chunk
    fn generate_proof(&self, chunk: &impl Chunk) -> AuthResult<Self::Proof>;
}

/// Authorization-specific errors
#[derive(Error, Debug)]
pub enum AuthError {
    #[error("Invalid proof: {0}")]
    InvalidProof(&'static str),

    #[error("Proof expired")]
    Expired,

    #[error("Authorization capacity exceeded")]
    CapacityExceeded,

    #[error("Invalid state: {0}")]
    InvalidState(&'static str),

    #[error("Crypto error: {0}")]
    Crypto(#[from] SignatureError),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

/// Type alias for Result with AuthError
pub type AuthResult<T> = std::result::Result<T, AuthError>;

// Helper methods for error creation
impl AuthError {
    pub fn invalid_proof(msg: &'static str) -> Self {
        Self::InvalidProof(msg)
    }

    pub fn invalid_state(msg: &'static str) -> Self {
        Self::InvalidState(msg)
    }
}
