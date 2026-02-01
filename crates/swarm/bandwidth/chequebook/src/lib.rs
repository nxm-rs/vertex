//! Chequebook types and signing for SWAP settlement.
//!
//! This crate provides the core types for SWAP chequebook-based settlement:
//!
//! - [`Cheque`] - An unsigned cheque commitment (EIP-712 typed data)
//! - [`SignedCheque`] - A signed cheque ready for transmission or cashing
//!
//! # EIP-712 Signing
//!
//! Cheques use EIP-712 typed data signing with the domain:
//! - Name: "Chequebook"
//! - Version: "1.0"
//! - ChainId: from SwarmSpec's chain
//!
//! The cheque type is:
//! ```text
//! Cheque(address chequebook,address beneficiary,uint256 cumulativePayout)
//! ```
//!
//! # Signing a Cheque
//!
//! ```ignore
//! use alloy_signer::SignerSync;
//!
//! let cheque = Cheque::new(chequebook, beneficiary, amount);
//! let hash = cheque.signing_hash(&spec);
//! let sig = signer.sign_hash_sync(&hash)?;
//! let signed = SignedCheque::from_signature(cheque, sig);
//! ```
//!
//! # Wire Format
//!
//! Cheques are serialized as JSON for transmission over the SWAP protocol,
//! matching Bee's format for interoperability.

pub mod cheque;

pub use cheque::{Cheque, ChequeExt, SignedCheque};

// Re-export commonly used types
pub use alloy_primitives::{Address, U256};
pub use bytes::Bytes;

/// Errors that can occur during cheque operations.
#[derive(Debug, thiserror::Error)]
pub enum ChequeError {
    /// Cheque signing is not yet implemented.
    #[error("cheque signing not implemented")]
    SigningNotImplemented,

    /// Failed to recover signer from signature.
    #[error("failed to recover signer: {0}")]
    SignatureRecovery(String),

    /// Cheque was signed by unexpected address.
    #[error("invalid signer: expected {expected}, got {actual}")]
    InvalidSigner { expected: Address, actual: Address },

    /// Cheque serialization/deserialization failed.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// Cheque amount is invalid.
    #[error("invalid cheque amount: {0}")]
    InvalidAmount(String),

    /// Chequebook contract error.
    #[error("chequebook error: {0}")]
    Chequebook(String),
}
