//! Errors surfaced by [`PullsyncBehaviour`](crate::PullsyncBehaviour) as failure
//! events.

use strum::IntoStaticStr;

/// Why a pullsync command against a peer failed.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum PullsyncFailure {
    /// The substream upgrade or framed exchange failed (negotiation, transport,
    /// or a malformed message from the peer).
    #[error("pullsync stream failed: {0}")]
    Stream(String),

    /// The peer kept the substream open past the per-page deadline.
    #[error("pullsync exchange timed out")]
    TimedOut,
}
