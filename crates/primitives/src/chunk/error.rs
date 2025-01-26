use alloy::primitives::SignatureError;
use alloy::signers::Error as SignerError;
use std::io;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ChunkError {
    #[error("Size error: {context} (size: {size}, limit: {limit})")]
    Size {
        context: &'static str,
        size: usize,
        limit: usize,
    },

    #[error("Invalid chunk format: {0}")]
    Format(&'static str),

    #[error("Verification failed: {context} (expected: {expected:?}, got: {got:?})")]
    Verification {
        context: &'static str,
        expected: String,
        got: String,
    },

    #[error("Crypto error: {0}")]
    Signature(#[from] SignatureError),

    #[error("Signer error: {0}")]
    Signer(#[from] SignerError),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Missing required field: {0}")]
    MissingField(&'static str),
}

// Type alias for Result
pub type Result<T> = std::result::Result<T, ChunkError>;

// Helper methods for error creation
impl ChunkError {
    pub fn size(context: &'static str, size: usize, limit: usize) -> Self {
        Self::Size {
            context,
            size,
            limit,
        }
    }

    pub fn format(msg: &'static str) -> Self {
        Self::Format(msg)
    }

    pub fn verification<T: std::fmt::Debug>(context: &'static str, expected: T, got: T) -> Self {
        Self::Verification {
            context,
            expected: format!("{:?}", expected),
            got: format!("{:?}", got),
        }
    }

    pub fn missing_field(field: &'static str) -> Self {
        Self::MissingField(field)
    }
}
