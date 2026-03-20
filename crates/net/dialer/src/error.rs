//! Error types for dial preparation.

/// Error returned by [`DialTracker::prepare_and_start`](crate::DialTracker::prepare_and_start).
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum PrepareError {
    #[error("no reachable addresses after filtering")]
    NoReachableAddresses,
    #[error("peer already pending or in-flight")]
    AlreadyTracked,
    #[error("peer in backoff")]
    InBackoff,
    #[error("peer is banned")]
    Banned,
}
