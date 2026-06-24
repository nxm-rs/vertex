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

/// Default percent of the payment-threshold headroom the outbound self-throttle
/// will consume. The throttle settles a peer past the early-payment trigger
/// before admitting a request, so the committed debit is pre-paid and never
/// crosses the remote disconnect line on its own; the full headroom can be paced
/// against without leaving a static margin, and running nearer the line settles
/// more often during a request so debt does not carry across to the next.
pub(crate) const DEFAULT_THROTTLE_ALLOWANCE_PERCENT: u8 = 100;
