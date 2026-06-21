//! SWAP settlement errors.

use alloy_primitives::{Address, U256};
use vertex_swarm_api::{Au, AuConversionError};

/// Errors that can occur during swap operations.
#[derive(Debug, Clone, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum SwapSettlementError {
    /// Service has stopped.
    #[error("swap service stopped")]
    ServiceStopped,

    /// Settlement already in progress with this peer.
    #[error("settlement already in progress")]
    SettlementInProgress,

    /// Network error.
    #[error("network error: {0}")]
    NetworkError(String),

    /// The peer's beneficiary has not been learned from the swap handshake yet.
    #[error("unknown beneficiary for peer")]
    UnknownBeneficiary,

    /// A cheque arrived from a peer whose swap identity has not been learned, so
    /// its issuer cannot be authenticated.
    #[error("no swap identity learned for peer; cannot authenticate cheque")]
    UnknownPeerIdentity,

    /// A received cheque names a beneficiary that is not our payout address.
    #[error("cheque beneficiary mismatch: expected {expected}, got {got}")]
    BeneficiaryMismatch {
        /// Our own beneficiary, the only address a cheque to us may name.
        expected: Address,
        /// The beneficiary the received cheque actually names.
        got: Address,
    },

    /// Cheque signing failed.
    #[error("cheque signing failed: {0}")]
    SigningFailed(String),

    /// A received cheque's signature did not recover to the expected issuer.
    #[error("cheque issuer mismatch: expected {expected}, recovered {recovered}")]
    IssuerMismatch {
        /// The chequebook issuer expected for this peer.
        expected: Address,
        /// The address actually recovered from the cheque signature.
        recovered: Address,
    },

    /// A received cheque did not increase the cumulative payout.
    #[error("cumulative payout did not increase: last {last}, received {received}")]
    NonIncreasingPayout {
        /// The cumulative payout of the last accepted cheque.
        last: U256,
        /// The cumulative payout of the received cheque.
        received: U256,
    },

    /// The incremental cheque amount did not fit the accounting unit type.
    #[error("cheque amount overflows accounting unit: {0}")]
    AmountOverflow(U256),

    /// A settlement amount was negative and has no cheque payout representation.
    #[error("settlement amount is negative: {0}")]
    NegativeAmount(Au),

    /// Cheque validation failed.
    #[error("cheque validation failed: {0}")]
    ValidationFailed(String),

    /// Accepting the cheque would push the peer's uncashed exposure past the
    /// per-peer bounce limit. Cashing is stubbed for v1, so the cap is hard:
    /// debt stops being settled for this peer until cashout confirms (#438).
    #[error("uncashed exposure at bounce limit: exposure {exposure}, limit {limit}")]
    ExposureLimit {
        /// The peer's current uncashed cumulative-payout exposure.
        exposure: U256,
        /// The configured per-peer bounce limit.
        limit: U256,
    },

    /// Chain backend not available for the requested cashout.
    #[error("chain backend not available")]
    NoChainBackend,
}

impl From<AuConversionError> for SwapSettlementError {
    fn from(err: AuConversionError) -> Self {
        match err {
            AuConversionError::U256TooLarge(value) => Self::AmountOverflow(value),
            AuConversionError::Negative(value) => Self::NegativeAmount(value),
        }
    }
}
