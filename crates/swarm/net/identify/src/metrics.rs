//! Metrics for the identify protocol.

use metrics::{counter, histogram};
use vertex_observability::{
    DURATION_SECONDS, HistogramBucketConfig, LabelValue,
    labels::{direction, outcome},
};

/// Histogram bucket configurations for identify metrics.
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[HistogramBucketConfig {
    suffix: "identify_duration_seconds",
    buckets: DURATION_SECONDS,
}];

/// Identify error classification for metrics labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum IdentifyErrorKind {
    Timeout,
    Apply,
}

/// Bounded classification of a remote peer's agent version for metrics labels.
///
/// The raw `agent_version` is attacker-controlled, so it must never be used
/// directly as a label value: an adversary could mint unlimited distinct label
/// values and blow up metric cardinality. This enum bounds the label to a small
/// fixed set, detected from a prefix of the agent string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum AgentKind {
    /// The Go reference implementation (agent prefix `bee`).
    Bee,
    /// This implementation (agent prefix `vertex`).
    Vertex,
    /// Any other or empty agent string.
    Other,
}

impl AgentKind {
    /// Classify a raw agent version string into a bounded label value.
    fn classify(agent_version: &str) -> Self {
        let trimmed = agent_version.trim();
        // Compare against fixed prefixes without slicing the input, since the
        // input is attacker-controlled and slicing at a fixed byte offset could
        // panic on a non-UTF-8-boundary. `as_bytes` comparison is ASCII-safe.
        if has_ascii_prefix_ignore_case(trimmed, b"vertex") {
            Self::Vertex
        } else if has_ascii_prefix_ignore_case(trimmed, b"bee") {
            Self::Bee
        } else {
            Self::Other
        }
    }
}

/// Record a received identify event with the remote peer's agent version.
///
/// Increments `identify_received_total` with `purpose` and a bounded
/// `agent_kind` label so that the distribution of agent kinds can be queried
/// per-swarm via `sum by (agent_kind) (identify_received_total{purpose="topology"})`.
/// The label is a fixed enum rather than the raw, peer-controlled string so that
/// an adversarial peer cannot inflate metric cardinality.
pub(crate) fn record_received(
    purpose: &'static str,
    agent_version: &str,
    duration: std::time::Duration,
) {
    let kind = AgentKind::classify(agent_version);

    // The full agent_version is attacker-controlled, so it never becomes a label
    // value. It is still useful for developers, so emit a length-bounded,
    // char-boundary-safe copy on the debug log only.
    tracing::debug!(
        purpose,
        agent_kind = kind.label_value(),
        agent_version = truncate_on_char_boundary(agent_version.trim(), 64),
        "identify received",
    );

    counter!(
        "identify_received_total",
        "purpose" => purpose,
        "agent_kind" => kind.label_value(),
    )
    .increment(1);

    histogram!(
        "identify_duration_seconds",
        "purpose" => purpose,
        "direction" => direction::INBOUND,
        "outcome" => outcome::SUCCESS,
    )
    .record(duration.as_secs_f64());
}

/// Record an outbound identify push event.
pub(crate) fn record_pushed(purpose: &'static str) {
    counter!("identify_pushed_total", "purpose" => purpose).increment(1);
}

/// Record an outbound identify sent event.
pub(crate) fn record_sent(purpose: &'static str) {
    counter!("identify_sent_total", "purpose" => purpose).increment(1);
}

/// Record an identify error.
pub(crate) fn record_error(
    purpose: &'static str,
    kind: IdentifyErrorKind,
    duration: std::time::Duration,
) {
    counter!(
        "identify_error_total",
        "purpose" => purpose,
        "kind" => kind.label_value(),
    )
    .increment(1);

    histogram!(
        "identify_duration_seconds",
        "purpose" => purpose,
        "direction" => direction::INBOUND,
        "outcome" => outcome::FAILURE,
    )
    .record(duration.as_secs_f64());
}

/// Return whether `s` starts with the ASCII `prefix`, case-insensitively.
///
/// Operates on bytes so it never slices `s` at a non-UTF-8 char boundary, which
/// matters because `s` is attacker-controlled. Only the fixed ASCII `prefix`
/// bytes are inspected.
fn has_ascii_prefix_ignore_case(s: &str, prefix: &[u8]) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Truncate `s` to at most `max` bytes without splitting a UTF-8 character.
///
/// Returns the longest prefix of `s` whose byte length is `<= max` and that ends
/// on a char boundary. Slicing `&s[..max]` directly would panic when `max` lands
/// in the middle of a multi-byte character, and `s` is attacker-controlled, so
/// this floor-to-char-boundary form is used instead. `str::floor_char_boundary`
/// is still unstable on the workspace MSRV, so the boundary is found by hand.
fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let end = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= max)
        .last()
        .unwrap_or(0);
    s.get(..end).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_known_agents() {
        assert_eq!(AgentKind::classify("vertex/0.1.0"), AgentKind::Vertex);
        assert_eq!(AgentKind::classify("Vertex/0.1.0"), AgentKind::Vertex);
        assert_eq!(AgentKind::classify("bee/2.3.0-abc123"), AgentKind::Bee);
        assert_eq!(AgentKind::classify("  bee/2.3.0  "), AgentKind::Bee);
    }

    #[test]
    fn classify_unknown_agents() {
        assert_eq!(AgentKind::classify(""), AgentKind::Other);
        assert_eq!(AgentKind::classify("   "), AgentKind::Other);
        assert_eq!(AgentKind::classify("go-ipfs/0.4"), AgentKind::Other);
        // A short string that cannot match any prefix must not panic.
        assert_eq!(AgentKind::classify("be"), AgentKind::Other);
    }

    #[test]
    fn classify_label_values_are_bounded() {
        assert_eq!(AgentKind::Bee.label_value(), "bee");
        assert_eq!(AgentKind::Vertex.label_value(), "vertex");
        assert_eq!(AgentKind::Other.label_value(), "other");
    }

    #[test]
    fn classify_does_not_panic_on_multibyte_straddling_offset() {
        // 63 ASCII bytes followed by a 4-byte emoji so a multi-byte character
        // straddles byte offset 64. An adversarial agent_version of this shape
        // must classify without panicking.
        let adversarial = format!("{}\u{1F600}", "a".repeat(63));
        assert_eq!(AgentKind::classify(&adversarial), AgentKind::Other);

        // The same shape carrying a known prefix still classifies cleanly.
        let bee_like = format!("bee/{}\u{1F600}", "a".repeat(60));
        assert_eq!(AgentKind::classify(&bee_like), AgentKind::Bee);
    }

    #[test]
    fn truncate_on_char_boundary_never_splits_a_character() {
        // A multi-byte character straddling the cap must not be split, and the
        // call must not panic.
        let s = format!("{}\u{1F600}", "a".repeat(63));
        let truncated = truncate_on_char_boundary(&s, 64);
        // The emoji begins at byte 63 and is 4 bytes wide, so the floor boundary
        // is byte 63: the leading run of "a" is kept, the emoji is dropped whole.
        assert_eq!(truncated.len(), 63);
        assert!(truncated.is_char_boundary(truncated.len()));
        assert_eq!(truncated, "a".repeat(63));
    }

    #[test]
    fn truncate_on_char_boundary_passes_short_strings_through() {
        assert_eq!(truncate_on_char_boundary("bee/2.3.0", 64), "bee/2.3.0");
        assert_eq!(truncate_on_char_boundary("", 64), "");
    }
}
