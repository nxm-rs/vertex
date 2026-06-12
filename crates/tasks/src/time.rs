//! Timer scheduling that works on native and in the browser.
//!
//! Native builds drive timers from the tokio timer driver. On
//! `wasm32-unknown-unknown` there is no tokio timer driver: tokio's timer
//! reaches the std monotonic clock, which is the unsupported-platform stub on
//! that target and panics at runtime. The browser build therefore schedules
//! timers through `setTimeout` via `gloo-timers`.
//!
//! Use [`sleep`] anywhere a task running in the wasm cone needs to wait for a
//! duration. It returns a `'static` future so it composes inside `tokio::select!`
//! the same way on both targets.

use std::future::Future;
use std::time::Duration;

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
