//! Lock-free exponential backoff for peer dial attempts.

mod backoff;

pub use backoff::{
    DEFAULT_BASE_BACKOFF_SECS, DEFAULT_MAX_BACKOFF_SECS, PeerBackoff, backoff_remaining,
};
