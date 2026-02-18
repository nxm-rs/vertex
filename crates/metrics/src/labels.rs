//! Common label constants for metrics.

/// Connection or request direction.
pub mod direction {
    pub const INBOUND: &str = "inbound";
    pub const OUTBOUND: &str = "outbound";
}

/// Operation outcome.
pub mod outcome {
    pub const SUCCESS: &str = "success";
    pub const FAILURE: &str = "failure";
}

/// Boolean value labels.
pub mod boolean {
    pub const TRUE: &str = "true";
    pub const FALSE: &str = "false";

    #[inline]
    pub const fn from_bool(b: bool) -> &'static str {
        if b { TRUE } else { FALSE }
    }
}

/// Cache lookup result.
pub mod cache {
    pub const HIT: &str = "hit";
    pub const MISS: &str = "miss";
}
