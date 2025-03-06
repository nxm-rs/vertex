//! Errors specific to chunk operations

use std::fmt::Debug;
use thiserror::Error;

/// Result type specific to chunk operations
pub type Result<T> = std::result::Result<T, ChunkError>;

/// Errors specific to chunk operations
#[derive(Error, Debug)]
pub enum ChunkError {
    /// Error when a chunk's size is invalid
    #[error("Size error: {context} (size: {size}, limit: {limit})")]
    Size {
        /// Context description
        context: &'static str,
        /// Actual size
        size: usize,
        /// Size limit
        limit: usize,
    },

    /// Error when a chunk's format is invalid
    #[error("Invalid chunk format: {0}")]
    Format(String),

    /// Error when a chunk's verification fails
    #[error("Verification failed: {context} (expected: {expected:?}, got: {got:?})")]
    Verification {
        /// Context description
        context: &'static str,
        /// Expected value
        expected: String,
        /// Actual value
        got: String,
    },

    /// Error when a chunk type is unknown
    #[error("Unknown chunk type: {0:#04x}")]
    UnknownType(u8),

    /// Error when a chunk type is invalid
    #[error("Invalid chunk type: {0:#04x}, valid range for custom chunks is 0xE0-0xEF")]
    InvalidType(u8),

    /// Error when an operation is unsupported for a chunk type
    #[error("Unsupported operation for chunk type: {0}")]
    UnsupportedOperation(String),

    /// Error when a required field is missing
    #[error("Missing required field: {0}")]
    MissingField(&'static str),

    /// Error when chunk cannot be parsed
    #[error("Parse error: {0}")]
    Parse(String),

    /// Registry error
    #[error("Registry error: {0}")]
    Registry(String),
}

impl ChunkError {
    /// Create a new size error
    pub fn size(context: &'static str, size: usize, limit: usize) -> Self {
        Self::Size {
            context,
            size,
            limit,
        }
    }

    /// Create a new format error
    pub fn format<S: Into<String>>(msg: S) -> Self {
        Self::Format(msg.into())
    }

    /// Create a new verification error
    pub fn verification<T: Debug, U: Debug>(context: &'static str, expected: T, got: U) -> Self {
        Self::Verification {
            context,
            expected: format!("{:?}", expected),
            got: format!("{:?}", got),
        }
    }

    /// Create a new parse error
    pub fn parse<S: Into<String>>(msg: S) -> Self {
        Self::Parse(msg.into())
    }

    /// Create a new registry error
    pub fn registry<S: Into<String>>(msg: S) -> Self {
        Self::Registry(msg.into())
    }

    /// Create a new invalid type error for custom chunks
    pub fn invalid_custom_type(type_id: u8) -> Self {
        Self::InvalidType(type_id)
    }
}
