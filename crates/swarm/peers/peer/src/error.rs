//! Error types for peer identity and multiaddr operations.

/// Errors from multiaddr serialization/deserialization.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum MultiAddrError {
    #[error("empty byte slice")]
    EmptyData,
    #[error("failed to read varint: {0}")]
    VarintError(#[from] std::io::Error),
    #[error("inconsistent data: expected {expected} bytes, got {actual}")]
    InconsistentLength { expected: u64, actual: usize },
    #[error("failed to parse multiaddr: {0}")]
    InvalidMultiaddr(#[from] libp2p::multiaddr::Error),
}

/// Errors from [`SwarmPeer`](crate::SwarmPeer) and
/// [`SwarmPeer`](crate::SwarmPeer) construction.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum SwarmPeerError {
    #[error("invalid signature: {0}")]
    InvalidSignature(#[from] alloy_primitives::SignatureError),
    #[error("signer error: {0}")]
    SignerError(#[from] alloy_signer::Error),
    #[error("computed overlay does not match claimed overlay")]
    InvalidOverlay,
    #[error("at least one multiaddr is required")]
    NoMultiaddrs,
    #[error("invalid multiaddr encoding: {0}")]
    InvalidMultiaddrEncoding(#[from] MultiAddrError),
    /// Timestamp is non-positive (bee `ErrTimestampInvalid` equivalent).
    ///
    /// This is a structural protocol violation, distinct from a peer whose
    /// clock has drifted: triage the two separately. Bee uses
    /// `ErrTimestampInvalid` for `<= 0`.
    #[error("timestamp must be strictly positive")]
    InvalidTimestamp,
    /// Timestamp lies outside the caller's clock-skew tolerance.
    ///
    /// Distinct from [`InvalidTimestamp`](Self::InvalidTimestamp); this only
    /// fires when the caller passed `Some(skew)` and the drift exceeds it.
    #[error("timestamp outside permitted clock-skew window")]
    TimestampOutsideSkewWindow,
    #[error("invalid chequebook address encoding")]
    InvalidChequebook,
}
