//! Time utilities for peer management.

use std::time::{SystemTime, UNIX_EPOCH};

/// Returns current Unix timestamp in seconds.
pub(crate) fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
