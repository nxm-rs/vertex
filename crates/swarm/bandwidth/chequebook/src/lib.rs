//! Chequebook types and signing for SWAP settlement.
//!
//! This crate provides the core types for SWAP chequebook-based settlement:
//!
//! - [`Cheque`] - An unsigned cheque commitment (EIP-712 typed data)
//! - [`SignedCheque`] - A signed cheque ready for transmission or cashing
//!
//! By default this is a pure, wasm-safe codec with no RPC stack, so client
//! nodes compile without an Ethereum provider. The optional `swap-chequebook`
//! feature adds [`chain::ChequebookContract`], the native on-chain client that
//! deploys, cashes, and reads chequebooks over a shared
//! `alloy_provider::Provider`. A node role that settles over SWAP (the storer)
//! turns the feature on. The chequebook owns its chain client because deploy and
//! cashout are SWAP settlement details, not generic chain access.
//!
//! # EIP-712 Signing
//!
//! Cheques use EIP-712 typed data signing with the domain:
//! - Name: "Chequebook"
//! - Version: "1.0"
//! - ChainId: the settlement chain, passed in by the caller as a
//!   [`alloy_chains::NamedChain`]
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
//! let hash = cheque.signing_hash(chain);
//! let sig = signer.sign_hash_sync(&hash)?;
//! let signed = SignedCheque::from_signature(cheque, sig);
//! ```
//!
//! # Wire format
//!
//! A [`SignedCheque`] travels on the swap protocol as a JSON object embedded in
//! a protobuf `bytes` field. The JSON is transport-only: the signature is EIP-712
//! over the cheque fields, not over the JSON bytes, so the encoding only needs to
//! be cross-implementation parseable and value-preserving, not byte-identical.
//! [`SignedCheque`] derives serde, so callers encode and decode with `serde_json`
//! directly. The shape follows the field-value conventions other Swarm nodes
//! emit: PascalCase keys, lowercase `0x`-hex addresses, `CumulativePayout` as a
//! bare decimal JSON number spanning the full 256-bit range, and `Signature` as
//! standard base64. This whole JSON path is slated for protobuf replacement,
//! tracked in issue #183.

#[cfg(feature = "swap-chequebook")]
pub mod chain;
pub mod cheque;

#[cfg(feature = "swap-chequebook")]
pub use chain::ChequebookContract;
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

    /// The signature is malleable: a non-canonical `v` byte or a high-`s`
    /// component. ECDSA is malleable, so only the low-`s` (EIP-2) form with a
    /// canonical recovery byte is accepted.
    #[error("non-canonical signature: {0}")]
    NonCanonicalSignature(&'static str),
}
