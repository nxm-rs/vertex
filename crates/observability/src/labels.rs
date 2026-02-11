//! Common label constants for metrics.
//!
//! Provides standardized label values used across the node infrastructure.
//! Protocol-specific labels belong in `vertex-swarm-observability`.
//!
//! # Example
//!
//! ```rust
//! use vertex_observability::labels::{direction, outcome};
//! use metrics::counter;
//!
//! counter!("requests_total", "direction" => direction::INBOUND, "outcome" => outcome::SUCCESS)
//!     .increment(1);
//! ```

/// Connection or request direction labels.
pub mod direction {
    /// Incoming connection/request initiated by remote peer.
    pub const INBOUND: &str = "inbound";
    /// Outgoing connection/request initiated by local node.
    pub const OUTBOUND: &str = "outbound";
}

/// Operation outcome labels.
pub mod outcome {
    /// Operation completed successfully.
    pub const SUCCESS: &str = "success";
    /// Operation failed.
    pub const FAILURE: &str = "failure";
}

/// Boolean value labels.
pub mod boolean {
    pub const TRUE: &str = "true";
    pub const FALSE: &str = "false";

    /// Convert bool to label.
    #[inline]
    pub const fn from_bool(b: bool) -> &'static str {
        if b { TRUE } else { FALSE }
    }
}

/// Cache-related labels.
pub mod cache {
    /// Cache lookup found the item.
    pub const HIT: &str = "hit";
    /// Cache lookup did not find the item.
    pub const MISS: &str = "miss";
}
