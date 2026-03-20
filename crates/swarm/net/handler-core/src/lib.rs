//! Shared handler core for protocol connection handlers.
//!
//! Provides [`HandlerCore`] which encapsulates the common pattern of:
//! - A bounded event queue (`VecDeque<E>`)
//! - A rate limiter for inbound stream throttling
//! - An outbound-pending flag to serialize outbound requests
//!
//! Protocol handlers (e.g., pingpong, hive) compose this struct instead of
//! duplicating the same fields and logic.

use std::collections::VecDeque;
use std::time::Duration;

use vertex_net_ratelimiter::RateLimiter;

/// Shared core for protocol connection handlers.
///
/// Manages a bounded event queue, rate limiter, and outbound serialization flag.
/// Protocol handlers embed this struct and delegate common operations to it.
pub struct HandlerCore<E> {
    /// Pending events to emit to the behaviour.
    pending_events: VecDeque<E>,
    /// Token bucket for inbound rate limiting.
    rate_limiter: RateLimiter,
    /// Whether an outbound substream request is in flight.
    outbound_pending: bool,
}

impl<E> HandlerCore<E> {
    /// Create a new handler core with the given rate limiter configuration.
    pub fn new(rate_limit_burst: u32, rate_limit_refill: Duration) -> Self {
        Self {
            pending_events: VecDeque::new(),
            rate_limiter: RateLimiter::new(rate_limit_burst, rate_limit_refill),
            outbound_pending: false,
        }
    }

    /// Pop the next pending event, if any.
    pub fn poll_pending(&mut self) -> Option<E> {
        self.pending_events.pop_front()
    }

    /// Push an event to the pending queue.
    pub fn push_event(&mut self, event: E) {
        self.pending_events.push_back(event);
    }

    /// Try to acquire a rate limiter token for an inbound stream.
    ///
    /// Returns `true` if the stream should be accepted, `false` if rate-limited.
    pub fn try_accept_inbound(&mut self) -> bool {
        self.rate_limiter.try_acquire()
    }

    /// Check whether an outbound request is currently in flight.
    pub fn outbound_pending(&self) -> bool {
        self.outbound_pending
    }

    /// Set the outbound-pending flag.
    pub fn set_outbound_pending(&mut self, pending: bool) {
        self.outbound_pending = pending;
    }
}
