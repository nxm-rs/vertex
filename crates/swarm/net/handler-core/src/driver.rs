//! Capped driver over a `FuturesUnordered` of boxed outcome futures.

use std::task::{Context, Poll};

use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};

/// Wraps a `FuturesUnordered<BoxFuture<'static, O>>` with a capacity cap. The cap
/// is advisory: the caller decides where to enforce it via [`has_capacity`]. The
/// wrapper adds capacity bookkeeping only and never buffers or defers a poll.
///
/// [`has_capacity`]: OutcomeDriver::has_capacity
pub struct OutcomeDriver<O> {
    inner: FuturesUnordered<BoxFuture<'static, O>>,
    cap: usize,
}

impl<O> OutcomeDriver<O> {
    /// Create an empty driver with the given capacity cap.
    pub fn new(cap: usize) -> Self {
        Self {
            inner: FuturesUnordered::new(),
            cap,
        }
    }

    /// Add a future to the set.
    pub fn push(&mut self, fut: BoxFuture<'static, O>) {
        self.inner.push(fut);
    }

    /// Whether the set holds fewer than `cap` futures.
    pub fn has_capacity(&self) -> bool {
        self.inner.len() < self.cap
    }

    /// Number of in-flight futures.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Poll for the next completed outcome. Delegates 1:1 to the underlying
    /// stream; it registers `cx` on every call and never buffers a resolved
    /// outcome.
    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<O>> {
        self.inner.poll_next_unpin(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future;

    use futures::future::poll_fn;
    use futures::task::noop_waker_ref;

    #[test]
    fn has_capacity_flips_at_cap() {
        let mut d: OutcomeDriver<u32> = OutcomeDriver::new(2);
        assert!(d.has_capacity());
        d.push(Box::pin(future::ready(1)));
        assert!(d.has_capacity());
        d.push(Box::pin(future::ready(2)));
        assert!(!d.has_capacity());
        assert_eq!(d.len(), 2);
    }

    #[tokio::test]
    async fn poll_next_yields_then_empties() {
        let mut d: OutcomeDriver<u32> = OutcomeDriver::new(4);
        d.push(Box::pin(future::ready(7)));

        let first = poll_fn(|cx| d.poll_next(cx)).await;
        assert_eq!(first, Some(7));

        // Empty set resolves to Ready(None).
        let mut cx = Context::from_waker(noop_waker_ref());
        assert!(matches!(d.poll_next(&mut cx), Poll::Ready(None)));
        assert!(d.is_empty());
    }
}
