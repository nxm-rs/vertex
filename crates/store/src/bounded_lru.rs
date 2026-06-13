//! Byte-bounded least-recently-used store.

use std::collections::HashMap;
use std::hash::Hash;

use parking_lot::Mutex;

/// A value that reports its resident byte size so the store can bound itself by
/// memory rather than by entry count.
pub trait ByteSized {
    /// The number of bytes this value occupies for budgeting purposes.
    fn byte_size(&self) -> usize;
}

/// Byte-bounded LRU store, generic over key and value.
///
/// Lossy by design: inserting past the budget evicts least-recently-used
/// entries until the new value fits. The budget caps resident memory directly
/// (the right bound for a mobile or browser client), unlike an entry-count
/// bound that ignores value size.
///
/// Recency is tracked with a monotonic logical clock, not a wall clock, so the
/// store needs no time source and stays trivially wasm-clean. A `get` or a
/// re-`insert` touches an entry, moving it to the most-recently-used end.
///
/// A value larger than the whole budget cannot be held: inserting it evicts
/// everything and then drops the value itself, leaving the store empty. This is
/// the correct lossy behaviour for a cache (it never exceeds its budget) and is
/// covered by a unit test.
pub struct BoundedLruStore<K: Eq + Hash, V: ByteSized> {
    inner: Mutex<Inner<K, V>>,
}

struct Inner<K: Eq + Hash, V: ByteSized> {
    /// Entries keyed by `K`, each carrying the logical tick it was last used.
    entries: HashMap<K, Entry<V>>,
    /// Sum of `byte_size()` across all resident values.
    current_bytes: usize,
    /// The configured ceiling on `current_bytes`.
    max_bytes: usize,
    /// Monotonic logical clock; every touch takes the next tick.
    clock: u64,
}

struct Entry<V> {
    value: V,
    last_used: u64,
    bytes: usize,
}

impl<K, V> BoundedLruStore<K, V>
where
    K: Eq + Hash + Clone,
    V: ByteSized + Clone,
{
    /// Create a store bounded to `max_bytes` of resident value bytes.
    #[must_use]
    pub fn with_budget(max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: HashMap::new(),
                current_bytes: 0,
                max_bytes,
                clock: 0,
            }),
        }
    }

    /// Fetch a clone of the stored value, touching its recency on a hit.
    pub fn get(&self, key: &K) -> Option<V> {
        let mut inner = self.inner.lock();
        let tick = inner.next_tick();
        let entry = inner.entries.get_mut(key)?;
        entry.last_used = tick;
        Some(entry.value.clone())
    }

    /// Insert a value, evicting least-recently-used entries until it fits.
    ///
    /// Re-inserting an existing key replaces its value and its byte accounting
    /// and touches its recency. A value that cannot fit even in an empty store
    /// is dropped after the store is cleared (lossy).
    pub fn insert(&self, key: K, value: V) {
        let mut inner = self.inner.lock();
        let bytes = value.byte_size();
        let tick = inner.next_tick();

        if let Some(old) = inner.entries.remove(&key) {
            inner.current_bytes = inner.current_bytes.saturating_sub(old.bytes);
        }

        // A value larger than the whole budget cannot be held; evicting would
        // empty the store and still not fit it, so drop it and keep the budget.
        if bytes > inner.max_bytes {
            inner.evict_until_fits(0);
            return;
        }

        let target = inner.max_bytes - bytes;
        inner.evict_until_fits(target);
        inner.current_bytes += bytes;
        inner.entries.insert(
            key,
            Entry {
                value,
                last_used: tick,
                bytes,
            },
        );
    }

    /// Whether a value is resident for `key` (does not touch recency).
    pub fn contains(&self, key: &K) -> bool {
        self.inner.lock().entries.contains_key(key)
    }

    /// Remove a value, freeing its budget.
    pub fn remove(&self, key: &K) {
        let mut inner = self.inner.lock();
        if let Some(old) = inner.entries.remove(key) {
            inner.current_bytes = inner.current_bytes.saturating_sub(old.bytes);
        }
    }

    /// The number of resident entries (test and metrics aid).
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().entries.len()
    }

    /// Whether the store holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().entries.is_empty()
    }

    /// The current resident byte total (test and metrics aid).
    #[must_use]
    pub fn current_bytes(&self) -> usize {
        self.inner.lock().current_bytes
    }
}

impl<K, V> Inner<K, V>
where
    K: Eq + Hash + Clone,
    V: ByteSized,
{
    /// Take the next logical tick.
    fn next_tick(&mut self) -> u64 {
        let tick = self.clock;
        self.clock = self.clock.wrapping_add(1);
        tick
    }

    /// Evict least-recently-used entries until `current_bytes <= target`.
    fn evict_until_fits(&mut self, target: usize) {
        while self.current_bytes > target {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(old) = self.entries.remove(&victim) {
                self.current_bytes = self.current_bytes.saturating_sub(old.bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, PartialEq, Eq, Debug)]
    struct Sized(Vec<u8>);

    impl ByteSized for Sized {
        fn byte_size(&self) -> usize {
            self.0.len()
        }
    }

    fn val(n: usize, byte: u8) -> Sized {
        Sized(vec![byte; n])
    }

    #[test]
    fn insert_get_contains_remove_round_trip() {
        let store = BoundedLruStore::with_budget(1024);
        store.insert(1u32, val(10, 0xaa));
        assert!(store.contains(&1));
        assert_eq!(store.get(&1), Some(val(10, 0xaa)));
        assert_eq!(store.current_bytes(), 10);
        store.remove(&1);
        assert!(!store.contains(&1));
        assert_eq!(store.get(&1), None);
        assert_eq!(store.current_bytes(), 0);
    }

    #[test]
    fn evicts_by_byte_budget_not_count() {
        // Budget fits two 40-byte values but not three.
        let store = BoundedLruStore::with_budget(100);
        store.insert(1u32, val(40, 1));
        store.insert(2u32, val(40, 2));
        store.insert(3u32, val(40, 3));
        // The least-recently-used (key 1) is evicted to make room.
        assert!(!store.contains(&1));
        assert!(store.contains(&2));
        assert!(store.contains(&3));
        assert!(store.current_bytes() <= 100);
    }

    #[test]
    fn recency_is_touched_on_get() {
        let store = BoundedLruStore::with_budget(100);
        store.insert(1u32, val(40, 1));
        store.insert(2u32, val(40, 2));
        // Touch key 1 so key 2 is now least-recently-used.
        let _ = store.get(&1);
        store.insert(3u32, val(40, 3));
        // Key 2 is evicted, key 1 survives because it was touched.
        assert!(store.contains(&1));
        assert!(!store.contains(&2));
        assert!(store.contains(&3));
    }

    #[test]
    fn reinsert_replaces_value_and_byte_accounting() {
        let store = BoundedLruStore::with_budget(100);
        store.insert(1u32, val(40, 1));
        store.insert(1u32, val(10, 9));
        assert_eq!(store.get(&1), Some(val(10, 9)));
        assert_eq!(store.current_bytes(), 10);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn value_larger_than_budget_is_not_held() {
        let store = BoundedLruStore::with_budget(50);
        store.insert(1u32, val(10, 1));
        store.insert(2u32, val(200, 2));
        // The oversized value evicts the store and is itself dropped.
        assert!(!store.contains(&2));
        assert!(store.is_empty());
        assert_eq!(store.current_bytes(), 0);
    }
}
