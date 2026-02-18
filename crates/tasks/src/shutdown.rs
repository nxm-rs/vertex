//! Helper for shutdown signals

use futures_util::{
    FutureExt,
    future::{FusedFuture, Shared},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::sync::oneshot;

/// A Future that resolves when the shutdown event has been fired.
#[derive(Debug)]
pub struct GracefulShutdown {
    shutdown: Shutdown,
    guard: Option<GracefulShutdownGuard>,
}

impl GracefulShutdown {
    pub(crate) const fn new(shutdown: Shutdown, guard: GracefulShutdownGuard) -> Self {
        Self {
            shutdown,
            guard: Some(guard),
        }
    }

    /// Returns a new shutdown future that ignores the returned [`GracefulShutdownGuard`].
    ///
    /// This just maps the return value of the future to `()`, it does not drop the guard.
    pub fn ignore_guard(self) -> impl Future<Output = ()> + Send + Sync + Unpin + 'static {
        self.map(drop)
    }
}

impl Future for GracefulShutdown {
    type Output = GracefulShutdownGuard;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        ready!(self.shutdown.poll_unpin(cx));
        Poll::Ready(
            self.get_mut()
                .guard
                .take()
                .expect("Future polled after completion"),
        )
    }
}

impl Clone for GracefulShutdown {
    fn clone(&self) -> Self {
        Self {
            shutdown: self.shutdown.clone(),
            guard: self
                .guard
                .as_ref()
                .map(|g| GracefulShutdownGuard::new(Arc::clone(&g.0))),
        }
    }
}

/// Tracks active graceful shutdown tasks with efficient wake-on-completion.
#[derive(Debug)]
pub(crate) struct GracefulShutdownCounter {
    count: AtomicUsize,
    mutex: Mutex<()>,
    condvar: Condvar,
}

impl GracefulShutdownCounter {
    pub(crate) fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
            mutex: Mutex::new(()),
            condvar: Condvar::new(),
        }
    }

    pub(crate) fn increment(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn decrement(&self) {
        let prev = self.count.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 {
            self.condvar.notify_all();
        }
    }

    pub(crate) fn load(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    /// Block until all graceful tasks complete. Returns `true`.
    pub(crate) fn wait(&self) -> bool {
        let mut guard = self.mutex.lock().unwrap();
        while self.count.load(Ordering::SeqCst) > 0 {
            guard = self.condvar.wait(guard).unwrap();
        }
        true
    }

    /// Block until all graceful tasks complete or timeout expires.
    /// Returns `true` if all tasks completed, `false` on timeout.
    pub(crate) fn wait_timeout(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut guard = self.mutex.lock().unwrap();
        while self.count.load(Ordering::SeqCst) > 0 {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (new_guard, result) = self.condvar.wait_timeout(guard, remaining).unwrap();
            guard = new_guard;
            if result.timed_out() && self.count.load(Ordering::SeqCst) > 0 {
                return false;
            }
        }
        true
    }
}

/// A guard that fires once dropped to signal the [`TaskManager`](crate::TaskManager) that the
/// [`GracefulShutdown`] has completed.
#[derive(Debug)]
#[must_use = "if unused the task will not be gracefully shutdown"]
pub struct GracefulShutdownGuard(pub(crate) Arc<GracefulShutdownCounter>);

impl GracefulShutdownGuard {
    pub(crate) fn new(counter: Arc<GracefulShutdownCounter>) -> Self {
        counter.increment();
        Self(counter)
    }
}

impl Drop for GracefulShutdownGuard {
    fn drop(&mut self) {
        self.0.decrement();
    }
}

/// A Future that resolves when the shutdown event has been fired.
#[derive(Debug, Clone)]
pub struct Shutdown(Shared<oneshot::Receiver<()>>);

impl Future for Shutdown {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let pin = self.get_mut();
        if pin.0.is_terminated() || pin.0.poll_unpin(cx).is_ready() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// Shutdown signal that fires either manually or on drop by closing the channel
#[derive(Debug)]
pub struct Signal(oneshot::Sender<()>);

impl Signal {
    /// Fire the signal manually. Best-effort: receiver may already be dropped during shutdown.
    pub fn fire(self) {
        let _ = self.0.send(());
    }
}

/// Create a channel pair that's used to propagate shutdown event
pub fn signal() -> (Signal, Shutdown) {
    let (sender, receiver) = oneshot::channel();
    (Signal(sender), Shutdown(receiver.shared()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::join_all;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_shutdown() {
        let (_signal, _shutdown) = signal();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_drop_signal() {
        let (signal, shutdown) = signal();

        tokio::task::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            drop(signal)
        });

        shutdown.await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_multi_shutdowns() {
        let (signal, shutdown) = signal();

        let mut tasks = Vec::with_capacity(100);
        for _ in 0..100 {
            let shutdown = shutdown.clone();
            let task = tokio::task::spawn(async move {
                shutdown.await;
            });
            tasks.push(task);
        }

        drop(signal);

        join_all(tasks).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_drop_signal_from_thread() {
        let (signal, shutdown) = signal();

        let _thread = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(500));
            drop(signal)
        });

        shutdown.await;
    }

    #[test]
    fn test_counter_wait_immediate() {
        let counter = GracefulShutdownCounter::new();
        assert!(counter.wait());
    }

    #[test]
    fn test_counter_wait_timeout_immediate() {
        let counter = GracefulShutdownCounter::new();
        assert!(counter.wait_timeout(Duration::from_millis(10)));
    }

    #[test]
    fn test_counter_decrement_wakes() {
        let counter = Arc::new(GracefulShutdownCounter::new());
        counter.increment();

        let c = Arc::clone(&counter);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            c.decrement();
        });

        assert!(counter.wait_timeout(Duration::from_secs(5)));
    }

    #[test]
    fn test_counter_timeout_expires() {
        let counter = Arc::new(GracefulShutdownCounter::new());
        counter.increment();
        assert!(!counter.wait_timeout(Duration::from_millis(50)));
        // Clean up
        counter.decrement();
    }
}
