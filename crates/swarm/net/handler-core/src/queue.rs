//! Bounded FIFO queue with an explicit, caller-owned drop policy.

use std::collections::VecDeque;

/// Bounded FIFO backed by a `VecDeque`. Mechanics only: the drop policy lives at
/// the call site, which warns or increments its own counter on rejection or
/// eviction. Wakerless by design; every producer edge is followed by a handler
/// poll.
pub struct BoundedQueue<T> {
    inner: VecDeque<T>,
    cap: usize,
}

impl<T> BoundedQueue<T> {
    /// Create an empty queue that holds at most `cap` items.
    pub fn new(cap: usize) -> Self {
        Self {
            inner: VecDeque::new(),
            cap,
        }
    }

    /// Enqueue, rejecting the newest item once full. Returns `Err(t)` when the
    /// queue already holds `cap` items so the caller can warn or count the drop.
    pub fn push(&mut self, t: T) -> Result<(), T> {
        if self.inner.len() >= self.cap {
            return Err(t);
        }
        self.inner.push_back(t);
        Ok(())
    }

    /// Enqueue unconditionally, evicting and returning the oldest item once full.
    pub fn push_evict_oldest(&mut self, t: T) -> Option<T> {
        let evicted = if self.inner.len() >= self.cap {
            self.inner.pop_front()
        } else {
            None
        };
        self.inner.push_back(t);
        evicted
    }

    /// Enqueue unconditionally, ignoring the cap. For internal state-machine
    /// events that must never be dropped.
    pub fn push_back(&mut self, t: T) {
        self.inner.push_back(t);
    }

    /// Pop the oldest item.
    pub fn pop(&mut self) -> Option<T> {
        self.inner.pop_front()
    }

    /// Number of queued items.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterate the queued items oldest-first.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.inner.iter()
    }

    /// Remove the item at `idx` (oldest is 0), returning it if present.
    pub fn remove(&mut self, idx: usize) -> Option<T> {
        self.inner.remove(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_newest_returns_item_at_cap() {
        let mut q = BoundedQueue::new(2);
        assert!(q.push(1).is_ok());
        assert!(q.push(2).is_ok());
        // At cap: reject-newest returns the item.
        assert_eq!(q.push(3), Err(3));
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.pop(), Some(2));
        assert!(q.is_empty());
    }

    #[test]
    fn evict_oldest_returns_evicted_and_holds_cap() {
        let mut q = BoundedQueue::new(2);
        assert_eq!(q.push_evict_oldest(1), None);
        assert_eq!(q.push_evict_oldest(2), None);
        // At cap: evicts and returns the oldest, len stays <= cap.
        assert_eq!(q.push_evict_oldest(3), Some(1));
        assert_eq!(q.len(), 2);
        assert_eq!(q.push_evict_oldest(4), Some(2));
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some(3));
        assert_eq!(q.pop(), Some(4));
    }

    #[test]
    fn remove_mid_queue() {
        let mut q = BoundedQueue::new(4);
        q.push_back(10);
        q.push_back(20);
        q.push_back(30);
        assert_eq!(q.remove(1), Some(20));
        assert_eq!(q.len(), 2);
        let rest: Vec<_> = q.iter().copied().collect();
        assert_eq!(rest, vec![10, 30]);
        assert_eq!(q.remove(5), None);
    }
}
