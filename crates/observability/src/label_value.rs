//! Type-safe conversion from domain types to metric label strings.
//!
//! The [`LabelValue`] trait provides a consistent way to convert domain types
//! (especially enums) into label strings for metrics. It integrates with
//! [strum](https://docs.rs/strum) for zero-boilerplate enum support.
//!
//! # Using with Strum (Recommended)
//!
//! Add `#[derive(strum::IntoStaticStr)]` to your enum and `LabelValue` is
//! automatically implemented via a blanket impl:
//!
//! ```rust
//! use strum::IntoStaticStr;
//! use vertex_observability::LabelValue;
//!
//! #[derive(IntoStaticStr)]
//! #[strum(serialize_all = "snake_case")]
//! pub enum ConnectionDirection {
//!     Inbound,   // → "inbound"
//!     Outbound,  // → "outbound"
//! }
//!
//! let dir = ConnectionDirection::Inbound;
//! assert_eq!(dir.label_value(), "inbound");
//!
//! // Use in metrics:
//! // counter!("connections", "direction" => dir.label_value()).increment(1);
//! ```
//!
//! # Strum Attributes
//!
//! Common strum attributes for metric labels:
//!
//! ```rust
//! use strum::IntoStaticStr;
//!
//! // snake_case is most common for metrics
//! #[derive(IntoStaticStr)]
//! #[strum(serialize_all = "snake_case")]
//! pub enum DisconnectReason {
//!     RemoteClosed,      // → "remote_closed"
//!     ConnectionError,   // → "connection_error"
//! }
//!
//! // Custom values when automatic conversion doesn't fit
//! #[derive(IntoStaticStr)]
//! pub enum PeerType {
//!     #[strum(serialize = "full")]
//!     FullNode,
//!     #[strum(serialize = "light")]
//!     LightNode,
//! }
//! ```
//!
//! # Manual Implementation
//!
//! For types that can't use strum, implement manually:
//!
//! ```rust
//! use vertex_observability::LabelValue;
//!
//! pub struct CustomType(u8);
//!
//! impl LabelValue for CustomType {
//!     fn label_value(&self) -> &'static str {
//!         match self.0 {
//!             0 => "zero",
//!             1 => "one",
//!             _ => "other",
//!         }
//!     }
//! }
//! ```

/// Convert a value to a metric label string.
///
/// This trait is automatically implemented for any type that implements
/// `Into<&'static str>` for references (which `strum::IntoStaticStr` provides).
pub trait LabelValue {
    /// Returns the label string representation.
    ///
    /// The returned string should be:
    /// - Lowercase with underscores (snake_case)
    /// - Short and descriptive
    /// - Consistent across the codebase
    fn label_value(&self) -> &'static str;
}

/// Blanket implementation for types deriving `strum::IntoStaticStr`.
///
/// `IntoStaticStr` provides `impl<'a> From<&'a MyEnum> for &'static str`,
/// which this blanket impl uses to automatically implement `LabelValue`.
impl<T> LabelValue for T
where
    for<'a> &'a T: Into<&'static str>,
{
    #[inline]
    fn label_value(&self) -> &'static str {
        self.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoStaticStr;

    #[derive(IntoStaticStr)]
    #[strum(serialize_all = "snake_case")]
    enum TestDirection {
        Inbound,
        Outbound,
    }

    #[derive(IntoStaticStr)]
    enum TestOutcome {
        #[strum(serialize = "success")]
        Ok,
        #[strum(serialize = "failure")]
        Err,
    }

    #[test]
    fn test_strum_snake_case() {
        assert_eq!(TestDirection::Inbound.label_value(), "inbound");
        assert_eq!(TestDirection::Outbound.label_value(), "outbound");
    }

    #[test]
    fn test_strum_custom_serialize() {
        assert_eq!(TestOutcome::Ok.label_value(), "success");
        assert_eq!(TestOutcome::Err.label_value(), "failure");
    }
}
