//! Welcome message newtype used in the handshake `Ack` payload.
//!
//! Bee bounds the welcome message at 140 *Unicode characters* (not bytes —
//! the wire encoding is UTF-8 but the count is the user-visible grapheme
//! approximation `char_count`). The newtype encodes that invariant once at
//! construction time and short-circuits all subsequent length checks.

use std::fmt;

/// Maximum length of a welcome message in Unicode characters.
pub const MAX_WELCOME_MESSAGE_CHARS: usize = 140;

/// Validated handshake welcome message (≤ [`MAX_WELCOME_MESSAGE_CHARS`] chars).
///
/// Construct via [`WelcomeMessage::new`] or [`WelcomeMessage::truncated`].
/// The empty message is the default and represents "no welcome message".
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct WelcomeMessage(String);

impl WelcomeMessage {
    /// Construct a `WelcomeMessage`, rejecting inputs that exceed the limit.
    ///
    /// Use [`Self::truncated`] when callers want a best-effort fit instead
    /// of a hard error.
    pub fn new<S: Into<String>>(s: S) -> Result<Self, WelcomeMessageError> {
        let s = s.into();
        let n = s.chars().count();
        if n > MAX_WELCOME_MESSAGE_CHARS {
            return Err(WelcomeMessageError::TooLong {
                actual: n,
                max: MAX_WELCOME_MESSAGE_CHARS,
            });
        }
        Ok(Self(s))
    }

    /// Construct a `WelcomeMessage`, silently truncating to the limit.
    ///
    /// Useful for advertising local-side identity where we trust the source
    /// but want to be defensive about length.
    pub fn truncated<S: Into<String>>(s: S) -> Self {
        let s = s.into();
        if s.chars().count() <= MAX_WELCOME_MESSAGE_CHARS {
            return Self(s);
        }
        let truncated: String = s.chars().take(MAX_WELCOME_MESSAGE_CHARS).collect();
        Self(truncated)
    }

    /// The empty welcome message.
    #[inline]
    pub fn empty() -> Self {
        Self(String::new())
    }

    /// Borrow the message as a string slice.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this message is the empty string.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Consume the newtype and return the inner `String`.
    #[inline]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for WelcomeMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for WelcomeMessage {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Errors from constructing a [`WelcomeMessage`].
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum WelcomeMessageError {
    /// Input length (Unicode chars) exceeded [`MAX_WELCOME_MESSAGE_CHARS`].
    #[error("welcome message too long: {actual} chars, max {max}")]
    TooLong { actual: usize, max: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        let m = WelcomeMessage::default();
        assert!(m.is_empty());
        assert_eq!(m.as_str(), "");
    }

    #[test]
    fn accepts_at_max_chars() {
        let s = "x".repeat(MAX_WELCOME_MESSAGE_CHARS);
        let m = WelcomeMessage::new(s.clone()).expect("at-max accepted");
        assert_eq!(m.as_str(), s);
    }

    #[test]
    fn rejects_over_max_chars() {
        let s = "x".repeat(MAX_WELCOME_MESSAGE_CHARS + 1);
        let err = WelcomeMessage::new(s).unwrap_err();
        assert!(matches!(
            err,
            WelcomeMessageError::TooLong {
                actual: 141,
                max: 140
            }
        ));
    }

    #[test]
    fn truncated_silently_caps() {
        let s = "x".repeat(MAX_WELCOME_MESSAGE_CHARS + 25);
        let m = WelcomeMessage::truncated(s);
        assert_eq!(m.as_str().chars().count(), MAX_WELCOME_MESSAGE_CHARS);
    }

    #[test]
    fn truncated_preserves_short_input() {
        let m = WelcomeMessage::truncated("hello");
        assert_eq!(m.as_str(), "hello");
    }

    #[test]
    fn multi_byte_char_counts_as_one() {
        // 4-byte char (UTF-8) counts as a single character for the limit.
        let s = "🐝".repeat(MAX_WELCOME_MESSAGE_CHARS);
        let m = WelcomeMessage::new(s.clone()).expect("at-max accepted");
        assert_eq!(m.as_str(), s);
    }
}
