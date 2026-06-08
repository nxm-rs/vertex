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
//! # Wire format
//!
//! A [`SignedCheque`] travels on the swap protocol as a JSON object embedded in
//! a protobuf `bytes` field. JSON is tolerated here only because the object
//! shape is fixed and must stay byte-identical to the live network so peers
//! interoperate. The codec is driven by `serde_json` over a fixed-order wire
//! struct: the keys are PascalCase, the addresses are lowercase `0x`-hex,
//! `CumulativePayout` is a bare decimal JSON number spanning the full 256-bit
//! range, and `Signature` is standard base64. See [`cheque`] for the codec and
//! the conformance vectors under `tests/` for the pinned bytes.

pub mod cheque;

pub use cheque::{Cheque, ChequeExt, SignedCheque};

// Re-export commonly used types
pub use alloy_primitives::{Address, U256};
pub use bytes::Bytes;

/// Errors that can occur during cheque operations.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ChequeError {
    /// Failed to recover signer from signature.
    #[error("failed to recover signer: {0}")]
    SignatureRecovery(String),

    /// Cheque was signed by unexpected address.
    #[error("invalid signer: expected {expected}, got {actual}")]
    InvalidSigner { expected: Address, actual: Address },

    /// The JSON object was not well-formed for a signed cheque.
    #[error("malformed cheque json: {0}")]
    MalformedJson(&'static str),

    /// The cheque could not be encoded to wire JSON.
    #[error("failed to encode cheque json: {0}")]
    Encode(&'static str),

    /// A field held a value the codec could not parse.
    #[error("invalid cheque field {field}: {reason}")]
    InvalidField {
        /// The JSON key whose value failed to parse.
        field: &'static str,
        /// Why the value was rejected.
        reason: &'static str,
    },
}
