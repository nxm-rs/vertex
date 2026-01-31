//! Default constants for bandwidth accounting.

/// Default refresh rate per second.
pub(crate) const DEFAULT_REFRESH_RATE: u64 = 4_500_000;

/// Default payment threshold.
pub(crate) const DEFAULT_PAYMENT_THRESHOLD: u64 = 13_500_000;

/// Default payment tolerance as a percentage.
pub(crate) const DEFAULT_PAYMENT_TOLERANCE_PERCENT: u64 = 25;

/// Default early payment trigger percentage.
pub(crate) const DEFAULT_EARLY_PAYMENT_PERCENT: u64 = 50;

/// Default scaling factor for client-only nodes.
pub(crate) const DEFAULT_CLIENT_ONLY_FACTOR: u64 = 10;
