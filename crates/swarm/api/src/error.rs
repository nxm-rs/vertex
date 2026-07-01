//! Error types for Swarm API operations.

use libp2p::multiaddr;
use nectar_primitives::ChunkAddress;
use std::string::String;
use vertex_swarm_primitives::OverlayAddress;

use crate::Au;

/// Errors raised while preparing a per-peer accounting decision.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum AccountingError {
    /// Peer has exceeded disconnect threshold.
    #[error("peer {peer} balance {balance} exceeds disconnect threshold {threshold}")]
    DisconnectThreshold {
        /// The peer whose balance breached the threshold.
        peer: OverlayAddress,
        /// The peer's current balance.
        balance: Au,
        /// The disconnect threshold that was exceeded.
        threshold: Au,
    },

    /// Operation would exceed payment threshold.
    #[error("peer {peer} balance {balance} exceeds payment threshold {threshold}")]
    PaymentThreshold {
        /// The peer whose balance breached the threshold.
        peer: OverlayAddress,
        /// The peer's current balance.
        balance: Au,
        /// The payment threshold that was exceeded.
        threshold: Au,
    },

    /// Peer not found.
    #[error("peer {0} not found")]
    PeerNotFound(OverlayAddress),

    /// Settlement failed.
    #[error("settlement failed: {0}")]
    SettlementFailed(String),

    /// Channel closed (service stopped).
    #[error("channel closed")]
    ChannelClosed,
}

impl AccountingError {
    vertex_metrics::impl_record_error!("accounting_errors_total");
}

/// Error type for Swarm API operations.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum SwarmError {
    /// Retrieval exhausted every reachable peer without serving the chunk.
    ///
    /// Forwarding retrieval has no authoritative negative on the wire, so this is
    /// not a claim of absence: the reachable entry points were tried and none
    /// served it, so a later request after reconnection or a topology change may
    /// still succeed.
    #[error("retrieval exhausted all reachable peers for chunk: {address}")]
    RetrievalExhausted {
        /// The address of the chunk that could not be retrieved.
        address: ChunkAddress,
    },

    /// No storer found for the chunk in proximity range.
    #[error("no storer found for chunk: {chunk_address}")]
    NoStorer {
        /// The chunk address that couldn't be stored.
        chunk_address: ChunkAddress,
    },

    /// A push completed but custody could not be confirmed.
    ///
    /// Every candidate returned a custody receipt that the local node could not
    /// judge: its neighbourhood view is not credible (the neighbourhood has not
    /// saturated yet), so the receipt's custody depth cannot be anchored against
    /// a trustworthy floor. This is distinct from success (no receipt was
    /// trusted) and from [`InvalidSignature`](Self::InvalidSignature) (no proven
    /// misbehaviour: the receipts may be honest, the local node simply cannot
    /// verify them). The push should be treated as unconfirmed and retried once
    /// the local view is credible.
    #[error("custody unconfirmed for chunk {chunk_address}: neighbourhood view not credible")]
    UnconfirmedCustody {
        /// The chunk whose custody could not be confirmed.
        chunk_address: ChunkAddress,
    },

    /// Every candidate peer failed for a chunk operation.
    #[error("all {attempts} candidate peers failed for chunk {address}")]
    AllPeersFailed {
        /// The chunk address the operation targeted.
        address: ChunkAddress,
        /// Number of peers attempted.
        attempts: usize,
        /// The error from the last attempt.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Invalid postage stamp signature.
    #[error("invalid stamp signature for {chunk_address}: {reason}")]
    InvalidSignature {
        /// The chunk whose stamp failed validation.
        chunk_address: ChunkAddress,
        /// Description of the signature validation failure.
        reason: String,
    },

    /// Storage operation failed.
    #[error("storage error: {message}")]
    Storage {
        /// Description of the storage failure.
        message: String,
        /// Original error, if available.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Network operation failed.
    #[error("network error: {message}")]
    Network {
        /// Description of the network failure.
        message: String,
        /// Original error, if available.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Peer disconnected or unavailable.
    #[error("peer unavailable{}: {reason}", peer.map(|p| format!(": {}", p)).unwrap_or_default())]
    PeerUnavailable {
        /// The peer that became unavailable, if known.
        peer: Option<OverlayAddress>,
        /// Description of why the peer is unavailable.
        reason: String,
    },

    /// Bandwidth limit exceeded (peer owes too much).
    #[error("bandwidth limit exceeded for {peer}: balance {balance} > threshold {threshold}")]
    BandwidthLimitExceeded {
        /// The peer whose bandwidth limit was exceeded.
        peer: OverlayAddress,
        /// Current balance with the peer.
        balance: i64,
        /// The threshold that was exceeded.
        threshold: i64,
    },

    /// Payment required but not provided or invalid.
    #[error("payment required: {reason}")]
    PaymentRequired {
        /// Description of the payment requirement.
        reason: String,
        /// Original error, if available.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Invalid chunk data.
    #[error("invalid chunk{}: {reason}", address.map(|a| format!(": {}", a)).unwrap_or_default())]
    InvalidChunk {
        /// The chunk address, if known.
        address: Option<ChunkAddress>,
        /// Description of why the chunk is invalid.
        reason: String,
    },

    /// Accounting operation failed.
    #[error("accounting error: {message}")]
    Accounting {
        /// Description of the accounting failure.
        message: String,
        /// Original error, if available.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// A typed accounting decision failed.
    ///
    /// Carries the [`AccountingError`] so its discriminant survives the boundary
    /// rather than collapsing into a single `accounting` label.
    #[error(transparent)]
    AccountingDecision(#[from] AccountingError),

    /// Internal error.
    #[error("internal error: {message}")]
    Internal {
        /// Description of the internal failure.
        message: String,
        /// Original error, if available.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}

/// Generate constructor pairs for SwarmError variants with `message + source` fields.
macro_rules! sourced_error_constructors {
    ($($fn_name:ident => $Variant:ident { $field:ident }),+ $(,)?) => {
        $(
            /// Create from a source error, preserving the error chain.
            pub fn $fn_name(source: impl std::error::Error + Send + Sync + 'static) -> Self {
                Self::$Variant {
                    $field: source.to_string(),
                    source: Some(Box::new(source)),
                }
            }

            paste::paste! {
                /// Create from a message string with no source error.
                pub fn [<$fn_name _msg>]($field: impl Into<String>) -> Self {
                    Self::$Variant {
                        $field: $field.into(),
                        source: None,
                    }
                }
            }
        )+
    };
}

impl SwarmError {
    sourced_error_constructors! {
        storage => Storage { message },
        network => Network { message },
        accounting => Accounting { message },
        internal => Internal { message },
        payment_required => PaymentRequired { reason },
    }

    vertex_metrics::impl_record_error!("swarm_errors_total");

    /// Whether this error represents a transient failure that may succeed on retry.
    ///
    /// Retryable errors include network issues, peer unavailability, and accounting
    /// failures. Non-retryable errors include invalid data, missing chunks, and
    /// configuration issues.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Network { .. }
                | Self::PeerUnavailable { .. }
                | Self::Accounting { .. }
                | Self::AccountingDecision { .. }
                | Self::NoStorer { .. }
                | Self::AllPeersFailed { .. }
                | Self::UnconfirmedCustody { .. }
        )
    }

    /// Whether this error indicates invalid input data.
    pub fn is_invalid_input(&self) -> bool {
        matches!(
            self,
            Self::InvalidChunk { .. } | Self::InvalidSignature { .. }
        )
    }
}

/// Result type for Swarm API operations.
pub type SwarmResult<T> = core::result::Result<T, SwarmError>;

/// Kind of address that failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigAddressKind {
    /// Listen address for P2P connections.
    ListenAddr,
    /// Bootnode address for initial peer discovery.
    Bootnode,
    /// NAT address for external advertisement.
    NatAddr,
    /// Trusted peer address.
    TrustedPeer,
}

impl core::fmt::Display for ConfigAddressKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ListenAddr => write!(f, "listen address"),
            Self::Bootnode => write!(f, "bootnode address"),
            Self::NatAddr => write!(f, "NAT address"),
            Self::TrustedPeer => write!(f, "trusted peer address"),
        }
    }
}

/// Error type for configuration validation.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ConfigError {
    /// Invalid multiaddress.
    #[error("invalid {kind} '{addr}': {source}")]
    InvalidAddress {
        /// The type of address that failed validation.
        kind: ConfigAddressKind,
        /// The invalid address string.
        addr: String,
        /// The parse error.
        #[source]
        source: multiaddr::Error,
    },

    /// The maximum-peers cap was set to zero. The transport enforces it as a
    /// hard cap on established connections, so zero would deny every
    /// connection and isolate the node.
    #[error("max peers must be at least 1; 0 would deny every connection")]
    ZeroMaxPeers,
}

/// Result type for configuration operations.
pub type ConfigResult<T> = core::result::Result<T, ConfigError>;

/// Error type for identity configuration validation.
///
/// Marked `#[non_exhaustive]` so new variants can be added without breaking
/// downstream matches.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[non_exhaustive]
#[strum(serialize_all = "snake_case")]
pub enum IdentityError {
    /// An ephemeral identity was configured for a node type that requires a
    /// persistent (keystore-backed) signing key. Bootnodes and storers have
    /// overlay addresses that are part of the network contract; restarting
    /// with a fresh random key changes the overlay and orphans the network
    /// (bootnode case) or the staked reservation (storer case).
    #[error(
        "{node_type} requires a persistent identity but was launched ephemeral; configure a keystore"
    )]
    EphemeralWhenPersistent {
        /// The node type whose persistence requirement was violated.
        node_type: crate::SwarmNodeType,
    },
}
