//! Error types for the headers protocol.

use std::convert::Infallible;

use libp2p::swarm::StreamUpgradeError;
use metrics::counter;
use strum::IntoStaticStr;
use vertex_metrics::LabelValue;

vertex_net_codec::protocol_error! {
    /// Error during headers exchange.
    pub enum HeadersError {}
}

/// Error from a headered protocol upgrade.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// Headers exchange failed.
    #[error("headers error: {0}")]
    Headers(#[from] HeadersError),

    /// Inner protocol error.
    #[error("protocol error: {0}")]
    Protocol(Box<dyn std::error::Error + Send + Sync>),
}

/// Error from a headered protocol at the connection handler level.
///
/// Flattens `StreamUpgradeError<ProtocolError>` into a single enum for
/// ergonomic matching and metrics recording in connection handlers.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum UpgradeError {
    /// Protocol negotiation timed out.
    #[error("timeout")]
    Timeout,

    /// Remote peer does not support the protocol.
    #[error("protocol negotiation failed")]
    NegotiationFailed,

    /// Transport I/O error.
    #[error("io error: {0}")]
    #[strum(serialize = "io_error")]
    Io(std::io::Error),

    /// Headers exchange failed before protocol code ran.
    #[error("headers error: {0}")]
    #[strum(serialize = "headers_error")]
    Headers(HeadersError),

    /// Inner protocol error (typically already tracked by protocol-level metrics).
    #[error("protocol error: {0}")]
    #[strum(serialize = "protocol_error")]
    Protocol(Box<dyn std::error::Error + Send + Sync>),
}

impl UpgradeError {
    /// Whether this error originated from the inner protocol.
    ///
    /// Protocol-level errors are already tracked by `ProtocolMetrics` in the
    /// headers crate. Handler-level metrics should skip these to avoid
    /// double-counting.
    pub fn is_protocol_error(&self) -> bool {
        matches!(self, Self::Protocol(_))
    }

    /// Record this error in handler-level metrics unconditionally.
    ///
    /// Emits `protocol_upgrade_errors_total{protocol, direction, reason}`.
    pub fn record(&self, protocol: &'static str, direction: &'static str) {
        counter!(
            "protocol_upgrade_errors_total",
            "protocol" => protocol,
            "direction" => direction,
            "reason" => self.label_value()
        )
        .increment(1);
    }

    /// Record this error in handler-level metrics, skipping protocol errors
    /// (which are already tracked by `ProtocolMetrics`).
    ///
    /// Emits `protocol_upgrade_errors_total{protocol, direction, reason}`.
    pub fn record_if_untracked(&self, protocol: &'static str, direction: &'static str) {
        if !self.is_protocol_error() {
            self.record(protocol, direction);
        }
    }

    /// Convert from a stream upgrade error, record metrics, and flatten to `ProtocolStreamError`.
    pub fn record_and_convert(
        error: impl Into<UpgradeError>,
        protocol: &'static str,
        direction: &'static str,
    ) -> ProtocolStreamError {
        let upgrade_error = error.into();
        upgrade_error.record_if_untracked(protocol, direction);
        ProtocolStreamError::from(upgrade_error)
    }
}

impl From<StreamUpgradeError<ProtocolError>> for UpgradeError {
    fn from(error: StreamUpgradeError<ProtocolError>) -> Self {
        match error {
            StreamUpgradeError::Timeout => Self::Timeout,
            StreamUpgradeError::NegotiationFailed => Self::NegotiationFailed,
            StreamUpgradeError::Io(e) => Self::Io(e),
            StreamUpgradeError::Apply(ProtocolError::Headers(e)) => Self::Headers(e),
            StreamUpgradeError::Apply(ProtocolError::Protocol(e)) => Self::Protocol(e),
        }
    }
}

impl From<ProtocolError> for UpgradeError {
    fn from(error: ProtocolError) -> Self {
        match error {
            ProtocolError::Headers(e) => Self::Headers(e),
            ProtocolError::Protocol(e) => Self::Protocol(e),
        }
    }
}

/// Unified error for headered protocols using [`FramedProto`](vertex_net_codec::FramedProto).
///
/// Covers codec-level errors (`ConnectionClosed`, `Protobuf`, `Io`) used in
/// `read()`/`write()`, and upgrade-level errors (`Timeout`, `NegotiationFailed`,
/// `Upgrade`) surfaced by the connection handler via `From<UpgradeError>`.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ProtocolStreamError {
    /// Connection closed before message was received.
    #[error("connection closed")]
    ConnectionClosed,

    /// Protobuf encoding/decoding error.
    #[error("protobuf error: {0}")]
    #[strum(serialize = "protobuf_error")]
    Protobuf(#[from] quick_protobuf_codec::Error),

    /// I/O error during stream operations.
    #[error("io error: {0}")]
    #[strum(serialize = "io_error")]
    Io(#[from] std::io::Error),

    /// Protocol negotiation timed out.
    #[error("timeout")]
    Timeout,

    /// Remote peer does not support the protocol.
    #[error("protocol negotiation failed")]
    NegotiationFailed,

    /// Protocol upgrade failed (headers or transport error).
    #[error("upgrade error: {0}")]
    #[strum(serialize = "upgrade_error")]
    Upgrade(String),
}

impl From<Infallible> for ProtocolStreamError {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}

impl From<vertex_net_codec::StreamClosed> for ProtocolStreamError {
    fn from(_: vertex_net_codec::StreamClosed) -> Self {
        Self::ConnectionClosed
    }
}

impl From<UpgradeError> for ProtocolStreamError {
    fn from(err: UpgradeError) -> Self {
        match err {
            UpgradeError::Timeout => Self::Timeout,
            UpgradeError::NegotiationFailed => Self::NegotiationFailed,
            UpgradeError::Io(e) => Self::Io(e),
            UpgradeError::Headers(e) => Self::Upgrade(e.to_string()),
            UpgradeError::Protocol(e) => Self::Upgrade(e.to_string()),
        }
    }
}
