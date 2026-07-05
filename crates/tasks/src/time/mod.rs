//! Timer scheduling that works on native and in the browser.
//!
//! Native builds drive timers from the tokio timer driver. On
//! `wasm32-unknown-unknown` there is no tokio timer driver: tokio's timer
//! reaches the std monotonic clock, which is the unsupported-platform stub on
//! that target and panics at runtime. The browser build therefore schedules
//! timers through `setTimeout` via `gloo-timers`.
//!
//! This module is the one import path for timer code in the wasm cone:
//!
//! - [`sleep`] waits for a duration; [`send_sleep`] and its re-armable
//!   [`SendDelay`] wrapper do the same on a `Send` future for timers held across
//!   an `await` inside a `Send`-bounded behaviour future.
//! - [`interval`] and [`interval_after`] build an [`Interval`] for periodic
//!   work, with [`Interval::poll_tick`] for behaviour poll loops and
//!   [`Interval::tick`] for async tasks.
//! - [`timeout`] bounds a future by a deadline and returns
//!   `Result<T, Elapsed>`.
//! - [`Instant`] is the timer-coherent monotonic clock.
//! - [`now_unix_secs`], [`now_unix_millis`], and [`now_unix_nanos`] are the
//!   runtime wall clock: Unix timestamps derived from the timer clock, so
//!   paused-clock tests and the sim move them together with timers.
//!
//! Two clock homes, one contract: this module is the runtime wall clock, and
//! all internal bookkeeping and persistence timestamps read here.
//! `vertex_util_runtime::time` is the platform wall clock, always real:
//! wire-visible timestamps, log and metric stamps, and any comparison against
//! a remote-produced timestamp read there instead.

mod interval;
mod wall;

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

pub use interval::{Interval, interval, interval_after};
pub use vertex_util_runtime::time::{Duration, SystemTime, UNIX_EPOCH};
pub use wall::{now_unix_millis, now_unix_nanos, now_unix_secs};

/// The monotonic clock that timers in this module are driven by.
///
/// On native this is tokio's clock, so `tokio::time::pause` and
/// `tokio::time::advance` move sleeps, intervals, timeouts, and instants
/// together in paused-time tests. This choice is load-bearing for the
/// `start_paused` tests in `vertex-swarm-topology`; do not swap it for the
/// web-time clock.
#[cfg(not(target_arch = "wasm32"))]
pub use tokio::time::Instant;

/// The monotonic clock that timers in this module are driven by.
///
/// On `wasm32` this is the browser performance clock via `web-time`, the same
/// clock the `gloo-timers` timer futures follow.
#[cfg(target_arch = "wasm32")]
pub use vertex_util_runtime::time::Instant;

/// A boxed, re-armable timer future.
///
/// Use this for struct fields that hold an armed [`sleep`]; do not define
/// per-crate siblings. The `Send` bound follows the platform executor via
/// [`crate::MaybeSendBoxFuture`]: `Send` on native, unbounded on `wasm32`
/// where the browser timer future is `!Send`.
pub type BoxTimerFuture = crate::MaybeSendBoxFuture<()>;

/// Wait for `duration`, then resolve.
///
/// On native targets this is `tokio::time::sleep`. On `wasm32` it is the
/// browser's `setTimeout` through `gloo-timers`, because the tokio timer driver
/// does not run there.
///
/// The returned future is `'static` and may be held across `await` points and
/// dropped early (cancelling the timer) on either target.
#[cfg(not(target_arch = "wasm32"))]
pub fn sleep(duration: Duration) -> impl Future<Output = ()> + 'static {
    tokio::time::sleep(duration)
}

/// Wait for `duration`, then resolve.
///
/// On native targets this is `tokio::time::sleep`. On `wasm32` it is the
/// browser's `setTimeout` through `gloo-timers`, because the tokio timer driver
/// does not run there.
///
/// The returned future is `'static` and may be held across `await` points and
/// dropped early (cancelling the timer) on either target.
#[cfg(target_arch = "wasm32")]
pub fn sleep(duration: Duration) -> impl Future<Output = ()> + 'static {
    // `gloo_timers` takes a `u32` millisecond count; saturate rather than wrap so
    // an absurdly long duration still produces a (very long) timer.
    let millis = u32::try_from(duration.as_millis()).unwrap_or(u32::MAX);
    gloo_timers::future::TimeoutFuture::new(millis)
}

/// Wait for `duration`, then resolve, on a future that is `Send` on both
/// targets.
///
/// Prefer [`sleep`] for timer code that stays on one task. Reach for this only
/// where the timer is held across an `await` inside a future that carries a
/// `Send` bound, as async-trait behaviour futures do: [`sleep`] is `!Send` on
/// `wasm32` and cannot cross those bounds.
///
/// On native this is `tokio::time::sleep`, so it follows the tokio clock and is
/// controllable under `tokio::time::pause` and `tokio::time::advance`. On
/// `wasm32` it is `futures_timer::Delay`, the `Send` browser timer, driven by
/// the wall clock.
#[cfg(not(target_arch = "wasm32"))]
pub fn send_sleep(duration: Duration) -> impl Future<Output = ()> + Send + 'static {
    tokio::time::sleep(duration)
}

/// Wait for `duration`, then resolve, on a future that is `Send` on both
/// targets.
///
/// Prefer [`sleep`] for timer code that stays on one task. Reach for this only
/// where the timer is held across an `await` inside a future that carries a
/// `Send` bound, as async-trait behaviour futures do: [`sleep`] is `!Send` on
/// `wasm32` and cannot cross those bounds.
///
/// On native this is `tokio::time::sleep`, so it follows the tokio clock and is
/// controllable under `tokio::time::pause` and `tokio::time::advance`. On
/// `wasm32` it is `futures_timer::Delay`, the `Send` browser timer, driven by
/// the wall clock.
#[cfg(target_arch = "wasm32")]
pub fn send_sleep(duration: Duration) -> impl Future<Output = ()> + Send + 'static {
    futures_timer::Delay::new(duration)
}

/// A re-armable [`send_sleep`] timer for struct fields.
///
/// Holds a boxed [`send_sleep`], so it is `Send` and `Unpin` on both targets: it
/// can be stored in a struct, polled through `Pin::new`, and re-armed in place
/// where tokio's `!Unpin` `Sleep` could not be. [`Self::reset`] re-arms it to
/// fire `duration` from the reset instant, the same as a fresh [`Self::new`].
pub struct SendDelay {
    inner: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl SendDelay {
    /// Arm a timer to fire after `duration`.
    pub fn new(duration: Duration) -> Self {
        Self {
            inner: Box::pin(send_sleep(duration)),
        }
    }

    /// Re-arm to fire `duration` from now, discarding any pending deadline.
    pub fn reset(&mut self, duration: Duration) {
        self.inner = Box::pin(send_sleep(duration));
    }
}

impl Future for SendDelay {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.inner.as_mut().poll(cx)
    }
}

/// Error returned by [`timeout`] when the deadline elapses before the wrapped
/// future completes.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("deadline elapsed")]
#[non_exhaustive]
pub struct Elapsed;

// `strum::IntoStaticStr` only derives for enums, so the static metric label
// conversions are written out by hand in the shape strum would generate.
impl From<Elapsed> for &'static str {
    fn from(_: Elapsed) -> Self {
        "elapsed"
    }
}

impl From<&Elapsed> for &'static str {
    fn from(_: &Elapsed) -> Self {
        "elapsed"
    }
}

/// Bound `future` by `duration`, resolving to `Err(Elapsed)` if the deadline
/// elapses first.
///
/// On native targets this is `tokio::time::timeout`, so it follows the tokio
/// clock and participates in paused-time tests.
#[cfg(not(target_arch = "wasm32"))]
pub fn timeout<F: Future>(
    duration: Duration,
    future: F,
) -> impl Future<Output = Result<F::Output, Elapsed>> {
    use futures_util::TryFutureExt;
    tokio::time::timeout(duration, future).map_err(|_| Elapsed)
}

/// Bound `future` by `duration`, resolving to `Err(Elapsed)` if the deadline
/// elapses first.
///
/// On `wasm32` the deadline is a browser [`sleep`] raced against the future,
/// because the tokio timer driver does not run there.
#[cfg(target_arch = "wasm32")]
pub fn timeout<F: Future>(
    duration: Duration,
    future: F,
) -> impl Future<Output = Result<F::Output, Elapsed>> {
    use futures_util::future::{Either, select};
    async move {
        let future = std::pin::pin!(future);
        let deadline = std::pin::pin!(sleep(duration));
        match select(future, deadline).await {
            Either::Left((output, _)) => Ok(output),
            Either::Right(((), _)) => Err(Elapsed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn timeout_returns_ok_before_deadline() {
        let result = timeout(Duration::from_secs(10), async {
            sleep(Duration::from_secs(1)).await;
            42
        })
        .await;
        assert_eq!(result, Ok(42));
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_returns_elapsed_after_deadline() {
        let result = timeout(Duration::from_secs(1), async {
            sleep(Duration::from_secs(10)).await;
            42
        })
        .await;
        assert_eq!(result, Err(Elapsed));
    }

    #[tokio::test(start_paused = true)]
    async fn instant_advances_under_tokio_advance() {
        let start = Instant::now();
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(start.elapsed(), Duration::from_secs(5));
    }

    #[test]
    fn elapsed_metric_label() {
        let label: &'static str = (&Elapsed).into();
        assert_eq!(label, "elapsed");
    }

    #[tokio::test(start_paused = true)]
    async fn send_sleep_completes_under_tokio_advance() {
        let start = Instant::now();
        send_sleep(Duration::from_secs(30)).await;
        assert_eq!(start.elapsed(), Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn send_delay_reset_re_arms_from_now() {
        let start = Instant::now();
        let mut timer = SendDelay::new(Duration::from_secs(5));
        tokio::time::advance(Duration::from_secs(2)).await;
        // Re-arm before it fires: the new deadline is measured from now, so the
        // total wait is the elapsed 2s plus the fresh 10s.
        timer.reset(Duration::from_secs(10));
        timer.await;
        assert_eq!(start.elapsed(), Duration::from_secs(12));
    }
}
