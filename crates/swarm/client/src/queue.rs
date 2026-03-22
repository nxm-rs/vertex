//! Bounded event queue with metric-based drop tracking.

use std::collections::VecDeque;

use tracing::warn;

/// A bounded event queue that drops events with a metric when full.
///
/// Used by both [`ClientBehaviour`](crate::ClientBehaviour) and
/// [`ClientHandler`](crate::handler::ClientHandler) to deduplicate the
/// capacity-check-and-drop-with-metric pattern.
pub(crate) struct BoundedEventQueue<E> {
    events: VecDeque<E>,
    capacity: usize,
    metric_name: &'static str,
}

impl<E> BoundedEventQueue<E> {
    /// Create a new bounded event queue.
    pub(crate) fn new(capacity: usize, metric_name: &'static str) -> Self {
        Self {
            events: VecDeque::new(),
            capacity,
            metric_name,
        }
    }

    /// Push an event, dropping it with a metric if the queue is full.
    ///
    /// Returns `true` if the event was queued, `false` if dropped.
    pub(crate) fn push(&mut self, event: E) -> bool {
        if self.events.len() >= self.capacity {
            warn!("Event queue full, dropping event");
            metrics::counter!(self.metric_name).increment(1);
            return false;
        }
        self.events.push_back(event);
        true
    }

    /// Push an event unconditionally (bypasses capacity check).
    ///
    /// Used for critical events (e.g. peer activation/disconnection)
    /// that must not be silently dropped.
    pub(crate) fn push_unchecked(&mut self, event: E) {
        self.events.push_back(event);
    }

    /// Pop the next pending event.
    pub(crate) fn pop(&mut self) -> Option<E> {
        self.events.pop_front()
    }

}
