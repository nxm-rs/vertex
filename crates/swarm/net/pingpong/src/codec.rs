//! Typed message model for the pingpong protocol.
//!
//! Wire format is still the underlying protobuf `Ping { greeting: string }` and
//! `Pong { response: string }` (see `vertex_swarm_net_proto::pingpong`), but at
//! the behaviour boundary we expose typed newtypes with a bounded length so the
//! rest of the stack never handles arbitrary strings.

use std::fmt;

/// Maximum length (in characters) of a pingpong greeting payload.
///
/// Bee accepts arbitrary strings; we cap them aggressively because pingpong is
/// a liveness/RTT probe, not a transport. The cap is checked against the
/// number of `char`s so a malicious peer cannot bypass it with multibyte
/// codepoints.
pub const MAX_GREETING_CHARS: usize = 64;

/// Errors returned when constructing a [`Greeting`] or [`GreetingEcho`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GreetingError {
    /// Payload exceeded the configured character cap.
    TooLong { len: usize, max: usize },
}

impl fmt::Display for GreetingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong { len, max } => {
                write!(f, "pingpong greeting too long: {len} chars (max {max})")
            }
        }
    }
}

impl std::error::Error for GreetingError {}

/// A pingpong greeting payload, bounded to [`MAX_GREETING_CHARS`] characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Greeting(String);

impl Greeting {
    /// Zero-length greeting. Always within the configured cap.
    pub const EMPTY: Self = Self(String::new());

    /// Construct a greeting, returning an error if the payload exceeds the cap.
    pub fn new(value: impl Into<String>) -> Result<Self, GreetingError> {
        Self::try_from(value.into())
    }

    /// Borrow the inner string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the greeting and return the inner string.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    /// Construct a [`GreetingEcho`] in the Bee-compatible wrapping `{greeting}` form.
    #[must_use]
    pub fn echo(&self) -> GreetingEcho {
        // greeting is at most MAX_GREETING_CHARS, so wrapping with two braces
        // is at most MAX_GREETING_CHARS + 2 chars, which is GreetingEcho::MAX_CHARS.
        GreetingEcho(format!("{{{}}}", self.0))
    }
}

impl TryFrom<String> for Greeting {
    type Error = GreetingError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let len = value.chars().count();
        if len > MAX_GREETING_CHARS {
            return Err(GreetingError::TooLong {
                len,
                max: MAX_GREETING_CHARS,
            });
        }
        Ok(Self(value))
    }
}

impl<'a> TryFrom<&'a str> for Greeting {
    type Error = GreetingError;

    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_string())
    }
}

impl fmt::Display for Greeting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A pingpong echo payload returned by a responder.
///
/// Bounded to [`MAX_GREETING_CHARS`] `+ 2` characters to accommodate the
/// `{greeting}` wrapping that bee uses on the response side.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GreetingEcho(String);

impl GreetingEcho {
    /// Maximum length of an echo payload (greeting + 2 wrapping braces).
    pub const MAX_CHARS: usize = MAX_GREETING_CHARS + 2;

    /// Construct an echo, returning an error if the payload exceeds the cap.
    pub fn new(value: impl Into<String>) -> Result<Self, GreetingError> {
        Self::try_from(value.into())
    }

    /// Borrow the inner string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the echo and return the inner string.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl TryFrom<String> for GreetingEcho {
    type Error = GreetingError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let len = value.chars().count();
        if len > Self::MAX_CHARS {
            return Err(GreetingError::TooLong {
                len,
                max: Self::MAX_CHARS,
            });
        }
        Ok(Self(value))
    }
}

impl<'a> TryFrom<&'a str> for GreetingEcho {
    type Error = GreetingError;

    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_string())
    }
}

impl fmt::Display for GreetingEcho {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Typed pingpong wire message at the behaviour boundary.
///
/// The on-wire encoding is still the underlying protobuf `Ping` / `Pong`
/// (see `vertex_swarm_net_proto::pingpong`), but constructing one of these
/// variants from a remote payload enforces the length cap.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PingpongMessage {
    Ping(Greeting),
    Pong(GreetingEcho),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn greeting_accepts_short_payload() {
        let g = Greeting::new("ping").expect("short greeting must parse");
        assert_eq!(g.as_str(), "ping");
    }

    #[test]
    fn greeting_rejects_over_cap() {
        let oversize = "a".repeat(MAX_GREETING_CHARS + 1);
        let err = Greeting::new(oversize).expect_err("must reject");
        assert!(matches!(
            err,
            GreetingError::TooLong { len, max }
                if len == MAX_GREETING_CHARS + 1 && max == MAX_GREETING_CHARS
        ));
    }

    #[test]
    fn greeting_cap_counts_chars_not_bytes() {
        // 32 four-byte codepoints = 128 bytes but only 32 chars: must pass.
        let s: String = std::iter::repeat_n('\u{1F600}', 32).collect();
        assert_eq!(s.len(), 128);
        assert!(Greeting::new(s).is_ok());

        // MAX + 1 four-byte codepoints: must fail.
        let s: String = std::iter::repeat_n('\u{1F600}', MAX_GREETING_CHARS + 1).collect();
        assert!(Greeting::new(s).is_err());
    }

    #[test]
    fn echo_wraps_greeting_with_braces() {
        let g = Greeting::new("hello").expect("must parse");
        assert_eq!(g.echo().as_str(), "{hello}");
    }

    #[test]
    fn greeting_echo_accepts_max_plus_two() {
        let s: String = std::iter::repeat_n('x', MAX_GREETING_CHARS + 2).collect();
        assert!(GreetingEcho::new(s).is_ok());
    }

    #[test]
    fn greeting_echo_rejects_over_cap() {
        let s: String = std::iter::repeat_n('x', MAX_GREETING_CHARS + 3).collect();
        assert!(GreetingEcho::new(s).is_err());
    }

    #[test]
    fn pingpong_message_variants_round_trip() {
        let ping = PingpongMessage::Ping(Greeting::new("hi").expect("must parse"));
        let pong = PingpongMessage::Pong(GreetingEcho::new("{hi}").expect("must parse"));
        assert!(matches!(ping, PingpongMessage::Ping(_)));
        assert!(matches!(pong, PingpongMessage::Pong(_)));
    }
}
