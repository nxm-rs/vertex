//! Receiver-side conflict resolution for signed peer-record timestamps.
//!
//! Every [`SwarmPeer`](crate::SwarmPeer) carries a signed wall-clock
//! [`Timestamp`]. When a fresh record arrives for an overlay we already hold,
//! the timestamp decides whether the new record supersedes the stored one or
//! is dropped as stale, replayed, or too frequent.
//!
//! This is purely receiver-side policy: it inspects no wire bytes beyond the
//! already-parsed timestamp and changes nothing on the wire. The structural
//! checks (non-positive timestamp, clock-skew window) live in
//! [`SwarmPeer::parse`](crate::SwarmPeer::parse); this module only resolves a
//! freshly parsed record against the existing stored record, so it does not
//! re-validate the `<= 0` / future-dated cases when a record has already been
//! parsed. The standalone in-future guard here covers callers that hand a raw
//! timestamp without going through `parse`.

use crate::Timestamp;
use std::time::Duration;

/// Records dated further than this into the future are rejected as
/// implausible (the remote clock is too far ahead of ours).
///
/// Vertex-tuned and tunable independently of the parse-side skew window.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_secs(60);

/// A gossiped record must improve on the stored timestamp by at least this
/// much to be accepted, throttling replay and flood of near-identical
/// re-advertisements between genuine address changes.
pub const MIN_UPDATE_INTERVAL: Duration = Duration::from_secs(300);

/// Reason a candidate record's timestamp was rejected.
///
/// The `strum::IntoStaticStr` snake-case rendering is the `reason` metric
/// label: `invalid`, `in_future`, `stale`, `too_soon`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum TimestampRejection {
    /// Timestamp is non-positive.
    #[error("timestamp must be strictly positive")]
    Invalid,
    /// Timestamp is dated more than [`MAX_CLOCK_SKEW`] into the future.
    #[error("timestamp dated too far into the future")]
    InFuture,
    /// Not strictly newer than the stored record.
    #[error("timestamp not newer than the stored record")]
    Stale,
    /// Newer, but within [`MIN_UPDATE_INTERVAL`] of the stored record.
    #[error("timestamp within the minimum update interval")]
    TooSoon,
}

impl TimestampRejection {
    /// Static `reason` label for metrics.
    #[inline]
    #[must_use]
    pub fn reason(self) -> &'static str {
        self.into()
    }
}

/// Decide whether a gossiped `candidate` record supersedes the `existing`
/// stored one.
///
/// This is the conflict-resolution rule for **second-hand gossip records**,
/// which may be stale or replayed copies relayed by another peer. First-hand
/// handshake records are not run through this: a live, identity-verified peer is
/// authoritative for its own addresses, so its record is accepted on the
/// structural validity that [`SwarmPeer::parse`](crate::SwarmPeer::parse) already
/// enforces (positive timestamp, clock-skew window).
///
/// `existing` is `None` when no prior record is held for the overlay, in which
/// case any structurally-valid record is accepted. `now` is the local
/// wall-clock time; pass it in (rather than reading the clock here) so the
/// function stays pure and wasm-friendly.
///
/// Semantics:
///
/// - non-positive candidate: [`TimestampRejection::Invalid`];
/// - candidate more than [`MAX_CLOCK_SKEW`] ahead of `now`:
///   [`TimestampRejection::InFuture`];
/// - no existing record, or a stored record with timestamp `<= 0` (legacy,
///   pre-timestamp): accepted;
/// - not strictly newer than `existing`: [`TimestampRejection::Stale`];
/// - strictly newer but within [`MIN_UPDATE_INTERVAL`] of `existing`:
///   [`TimestampRejection::TooSoon`] (throttles replay and re-advertisement
///   flood between genuine address changes).
pub fn check_timestamp(
    candidate: Timestamp,
    existing: Option<Timestamp>,
    now: Timestamp,
) -> Result<(), TimestampRejection> {
    if candidate.get() <= 0 {
        return Err(TimestampRejection::Invalid);
    }

    let skew = i64::try_from(MAX_CLOCK_SKEW.as_secs()).unwrap_or(i64::MAX);
    if candidate.get() > now.get().saturating_add(skew) {
        return Err(TimestampRejection::InFuture);
    }

    // First record for this overlay, or a legacy stored record that predates
    // signed timestamps: accept unconditionally.
    let existing = match existing {
        Some(ts) if ts.get() > 0 => ts,
        _ => return Ok(()),
    };

    if candidate.get() <= existing.get() {
        return Err(TimestampRejection::Stale);
    }
    let min_interval = i64::try_from(MIN_UPDATE_INTERVAL.as_secs()).unwrap_or(i64::MAX);
    if candidate.get() <= existing.get().saturating_add(min_interval) {
        return Err(TimestampRejection::TooSoon);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const NOW: i64 = 1_700_000_000;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_seconds(secs)
    }

    fn now() -> Timestamp {
        ts(NOW)
    }

    #[test]
    fn no_existing_record_accepts() {
        assert!(check_timestamp(ts(NOW), None, now()).is_ok());
    }

    #[test]
    fn legacy_zero_existing_is_overwritten() {
        // A stored record with timestamp 0 (pre-timestamp) is always replaced.
        assert!(
            check_timestamp(ts(NOW), Some(ts(0)), now()).is_ok(),
            "gossip should overwrite a legacy zero-timestamp record"
        );
    }

    #[test]
    fn non_positive_candidate_is_invalid() {
        let err = check_timestamp(ts(0), Some(ts(NOW - 1000)), now()).unwrap_err();
        assert_eq!(err, TimestampRejection::Invalid);
        assert_eq!(err.reason(), "invalid");

        let err = check_timestamp(ts(-5), None, now()).unwrap_err();
        assert_eq!(err, TimestampRejection::Invalid);
    }

    #[test]
    fn in_future_beyond_skew_is_rejected() {
        let skew = MAX_CLOCK_SKEW.as_secs() as i64;
        // Exactly at the boundary is accepted.
        assert!(
            check_timestamp(ts(NOW + skew), None, now()).is_ok(),
            "+MAX_CLOCK_SKEW boundary is accepted"
        );
        // One second past the boundary is rejected.
        let err = check_timestamp(ts(NOW + skew + 1), None, now()).unwrap_err();
        assert_eq!(err, TimestampRejection::InFuture);
        assert_eq!(err.reason(), "in_future");
    }

    #[test]
    fn gossip_accepts_strictly_newer_beyond_interval() {
        let existing = ts(NOW - 10_000);
        let interval = MIN_UPDATE_INTERVAL.as_secs() as i64;
        let candidate = ts(existing.get() + interval + 1);
        assert!(check_timestamp(candidate, Some(existing), now()).is_ok());
    }

    #[test]
    fn gossip_rejects_equal_as_stale() {
        let existing = ts(NOW - 1000);
        let err = check_timestamp(existing, Some(existing), now()).unwrap_err();
        assert_eq!(err, TimestampRejection::Stale);
        assert_eq!(err.reason(), "stale");
    }

    #[test]
    fn gossip_rejects_older_as_stale() {
        let existing = ts(NOW - 1000);
        let older = ts(existing.get() - 500);
        let err = check_timestamp(older, Some(existing), now()).unwrap_err();
        assert_eq!(err, TimestampRejection::Stale);
    }

    #[test]
    fn gossip_rejects_within_interval_as_too_soon() {
        let existing = ts(NOW - 10_000);
        let interval = MIN_UPDATE_INTERVAL.as_secs() as i64;
        // Strictly newer but inside the interval: too_soon.
        let candidate = ts(existing.get() + interval);
        let err = check_timestamp(candidate, Some(existing), now()).unwrap_err();
        assert_eq!(err, TimestampRejection::TooSoon);
        assert_eq!(err.reason(), "too_soon");

        // One second inside the interval is still too soon.
        let candidate = ts(existing.get() + 1);
        let err = check_timestamp(candidate, Some(existing), now()).unwrap_err();
        assert_eq!(err, TimestampRejection::TooSoon);
    }
}
