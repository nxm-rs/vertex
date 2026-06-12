//! Lazily armed periodic interval for native and wasm.

use std::fmt;
use std::task::{Context, Poll};

use super::{BoxTimerFuture, Duration, sleep};

/// Periodic interval whose underlying timer is created on first poll.
///
/// The tick is a re-armable boxed [`sleep`] future rather than a
/// `tokio::time::Interval` so the same code runs on wasm32, where tokio's
/// timer driver is absent. The timer is armed lazily on the first
/// [`Self::poll_tick`], so an `Interval` can be constructed without a running
/// runtime (libp2p behaviours construct intervals before the swarm polls), and
/// re-armed after each fire. Missed-tick behaviour is therefore "delay": a
/// late poll re-arms a full period after the fire rather than bursting to
/// catch up like tokio's default.
///
/// Re-arming boxes a fresh sleep each tick, one allocation per tick, which is
/// negligible at second-scale cadence; do not "optimize" this into a
/// cfg-gated `tokio::time::Interval`.
pub struct Interval {
    period: Duration,
    /// Delay before the first armed timer: `period` for [`interval`], the
    /// caller-provided delay for [`interval_after`].
    first_delay: Duration,
    timer: Option<BoxTimerFuture>,
    /// When set, the first poll readies a tick before any wait, matching
    /// `tokio::time::interval`. Set by [`interval`], not by [`interval_after`].
    first_tick_pending: bool,
}

/// Creates an [`Interval`] whose first tick fires immediately, then every
/// `period`.
pub fn interval(period: Duration) -> Interval {
    Interval {
        period,
        first_delay: period,
        timer: None,
        first_tick_pending: true,
    }
}

/// Creates an [`Interval`] whose first tick fires after `delay`, then every
/// `period`.
pub fn interval_after(delay: Duration, period: Duration) -> Interval {
    Interval {
        period,
        first_delay: delay,
        timer: None,
        first_tick_pending: false,
    }
}

impl Interval {
    /// Poll for the next tick, arming the timer on first use and re-arming it
    /// after each fire. For use inside `NetworkBehaviour` poll loops.
    ///
    /// Returning `Ready` on the immediate first tick without registering a
    /// waker is safe only because callers re-poll in a loop after a `Ready`.
    pub fn poll_tick(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        // The first tick of `interval` is immediate, matching
        // `tokio::time::interval`.
        if self.first_tick_pending {
            self.first_tick_pending = false;
            self.timer = Some(Box::pin(sleep(self.period)));
            return Poll::Ready(());
        }
        let first_delay = self.first_delay;
        let timer = self
            .timer
            .get_or_insert_with(|| Box::pin(sleep(first_delay)));
        match timer.as_mut().poll(cx) {
            Poll::Ready(()) => {
                self.timer = Some(Box::pin(sleep(self.period)));
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }

    /// Completes when the next tick fires. For use in async tasks.
    ///
    /// Cancel-safe: the armed timer lives in `self`, so dropping the returned
    /// future (for example from a `select!` arm) does not lose the tick.
    pub async fn tick(&mut self) {
        std::future::poll_fn(|cx| self.poll_tick(cx)).await
    }

    /// The configured period between ticks.
    pub const fn period(&self) -> Duration {
        self.period
    }
}

impl fmt::Debug for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Interval")
            .field("period", &self.period)
            .field("first_delay", &self.first_delay)
            .field("armed", &self.timer.is_some())
            .field("first_tick_pending", &self.first_tick_pending)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Instant;

    #[tokio::test(start_paused = true)]
    async fn interval_first_tick_is_immediate() {
        let mut ticker = interval(Duration::from_secs(5));
        let start = Instant::now();
        ticker.tick().await;
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn interval_subsequent_ticks_are_period_apart() {
        let mut ticker = interval(Duration::from_secs(5));
        ticker.tick().await;
        let start = Instant::now();
        ticker.tick().await;
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        ticker.tick().await;
        assert_eq!(start.elapsed(), Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn interval_after_first_tick_is_delayed() {
        let mut ticker = interval_after(Duration::from_secs(2), Duration::from_secs(5));
        let start = Instant::now();
        ticker.tick().await;
        assert_eq!(start.elapsed(), Duration::from_secs(2));
        ticker.tick().await;
        assert_eq!(start.elapsed(), Duration::from_secs(7));
    }

    #[test]
    fn period_returns_configured_period() {
        let ticker = interval(Duration::from_secs(3));
        assert_eq!(ticker.period(), Duration::from_secs(3));
        let ticker = interval_after(Duration::from_secs(1), Duration::from_secs(4));
        assert_eq!(ticker.period(), Duration::from_secs(4));
    }
}
