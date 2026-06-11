//! Error type for DNS-over-HTTPS dnsaddr resolution.

/// Errors that can occur while resolving dnsaddr records over DoH.
///
/// The `reason` label for metrics is derived through `strum::IntoStaticStr` so
/// the variant name round-trips into observability with no manual mapping.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[non_exhaustive]
pub enum DohError {
    /// The HTTP request to the DoH endpoint failed (network, CORS, or status).
    #[error("DoH request failed: {0}")]
    Request(String),

    /// The DoH response body was not valid DNS-JSON.
    #[error("DoH response parse failed: {0}")]
    Parse(String),

    /// Resolution completed but produced no browser-dialable leaves.
    #[error("dnsaddr resolution produced no browser-dialable leaves")]
    EmptyResolution,
}
