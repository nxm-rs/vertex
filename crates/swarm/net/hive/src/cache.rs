//! Sharded cache of recently validated peer records.
//!
//! Inbound gossip batches from different connections are validated
//! concurrently on blocking threads, and every record consults this cache to
//! skip a redundant ECDSA recovery. A single lock around the whole cache
//! serializes those lookups, so the keyspace is split into shards, each with
//! its own mutex and an independent LRU list. Concurrent batches contend only
//! when two records map to the same shard.
//!
//! Shard selection uses the last byte of the overlay address. Overlay
//! addresses are hash outputs, so any byte is uniformly distributed across
//! arbitrary peers, but peers in the local neighborhood share a leading
//! prefix; the last byte stays uniform even for them.
//!
//! See `benches/peer_cache.rs` for the concurrent-throughput comparison
//! against a single mutex, a read-write lock, and a sharded map with
//! timestamp aging.

use alloy_primitives::Signature;
use hashlink::LruCache;
use parking_lot::Mutex;
use vertex_swarm_peer::{SwarmAddress, SwarmPeer};

/// Maximum validated peers to cache across all shards (bounds memory).
pub(crate) const PEER_CACHE_CAPACITY: usize = 256;

/// Number of independently locked shards. A power of two so shard selection
/// is a bit mask.
const SHARD_COUNT: usize = 8;

const _: () = {
    assert!(
        SHARD_COUNT.is_power_of_two(),
        "shard mask requires a power-of-two shard count"
    );
    assert!(
        PEER_CACHE_CAPACITY.is_multiple_of(SHARD_COUNT),
        "capacity must split evenly across shards"
    );
};

/// Sharded LRU cache of recently validated peers, keyed by overlay address.
///
/// The capacity bound is global ([`PEER_CACHE_CAPACITY`]); each shard holds
/// an equal slice of it and ages its entries with its own LRU list, so
/// eviction is approximate global LRU.
pub(crate) struct PeerCache {
    shards: [Mutex<LruCache<SwarmAddress, SwarmPeer>>; SHARD_COUNT],
}

impl Default for PeerCache {
    fn default() -> Self {
        Self {
            shards: core::array::from_fn(|_| {
                Mutex::new(LruCache::new(PEER_CACHE_CAPACITY / SHARD_COUNT))
            }),
        }
    }
}

impl PeerCache {
    /// Shard owning the given overlay address.
    #[allow(clippy::indexing_slicing)] // byte 31 of a 32-byte address; index masked below SHARD_COUNT
    fn shard(&self, overlay: &SwarmAddress) -> &Mutex<LruCache<SwarmAddress, SwarmPeer>> {
        let index = usize::from(overlay.0[31]) & (SHARD_COUNT - 1);
        &self.shards[index]
    }

    /// Look up a previously validated record, returning a clone only when the
    /// cached record carries the same signature.
    ///
    /// A hit refreshes the entry's LRU position within its shard. A cached
    /// record with a different signature is treated as a miss; the caller
    /// re-validates and overwrites via [`Self::insert`].
    pub(crate) fn get(&self, overlay: &SwarmAddress, signature: &Signature) -> Option<SwarmPeer> {
        let mut shard = self.shard(overlay).lock();
        shard
            .get(overlay)
            .filter(|cached| cached.signature() == signature)
            .cloned()
    }

    /// Store a validated record, evicting the least recently used entry of
    /// its shard when the shard is full.
    pub(crate) fn insert(&self, overlay: SwarmAddress, peer: SwarmPeer) {
        self.shard(&overlay).lock().insert(overlay, peer);
    }

    /// Total number of cached records across all shards.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.shards.iter().map(|shard| shard.lock().len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256};
    use vertex_swarm_peer::{Nonce, Timestamp};

    use super::*;

    fn signature(byte: u8) -> Signature {
        let mut raw = [byte; 65];
        raw[64] = 0; // valid recovery id (parity)
        Signature::from_raw(&raw).unwrap()
    }

    fn overlay(index: usize) -> SwarmAddress {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
        // Spread the low byte so shards fill evenly.
        bytes[31] = (index % 251) as u8;
        SwarmAddress::from(B256::from(bytes))
    }

    fn peer(index: usize, sig: Signature) -> SwarmPeer {
        SwarmPeer::from_parts(
            Vec::new(),
            sig,
            overlay(index),
            Nonce::new([0u8; 32]),
            Timestamp::from_seconds(0),
            None,
            Address::ZERO,
        )
    }

    #[test]
    fn get_returns_only_signature_matches() {
        let cache = PeerCache::default();
        let sig = signature(2);
        cache.insert(overlay(1), peer(1, sig));

        assert!(cache.get(&overlay(1), &sig).is_some());
        assert!(cache.get(&overlay(1), &signature(3)).is_none());
        assert!(cache.get(&overlay(2), &sig).is_none());
    }

    #[test]
    fn insert_deduplicates_by_overlay() {
        let cache = PeerCache::default();
        let old = signature(2);
        let new = signature(3);
        cache.insert(overlay(1), peer(1, old));
        cache.insert(overlay(1), peer(1, new));

        assert_eq!(cache.len(), 1);
        assert!(cache.get(&overlay(1), &old).is_none());
        assert!(cache.get(&overlay(1), &new).is_some());
    }

    #[test]
    fn capacity_is_bounded_and_recently_used_survive() {
        let cache = PeerCache::default();
        let sig = signature(2);

        // Insert a hot set, then keep it warm while flooding with three times
        // the total capacity of one-shot entries.
        let hot: Vec<SwarmAddress> = (0..16).map(overlay).collect();
        for (i, key) in hot.iter().enumerate() {
            cache.insert(*key, peer(i, sig));
        }
        for i in 16..(16 + PEER_CACHE_CAPACITY * 3) {
            cache.insert(overlay(i), peer(i, sig));
            for key in &hot {
                assert!(cache.get(key, &sig).is_some(), "hot entry evicted");
            }
        }

        assert!(cache.len() <= PEER_CACHE_CAPACITY);
    }

    /// Concurrency stress: hammer overlapping keys from many threads and
    /// assert the dedup and capacity invariants hold under contention.
    #[test]
    fn concurrent_access_keeps_invariants() {
        const THREADS: usize = 8;
        const KEYS: usize = PEER_CACHE_CAPACITY * 2;
        const ROUNDS: usize = 50;

        let cache = PeerCache::default();
        let sig = signature(2);

        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let cache = &cache;
                let sig = &sig;
                scope.spawn(move || {
                    for round in 0..ROUNDS {
                        for i in 0..KEYS {
                            // Stagger threads so gets and inserts interleave
                            // on the same keys.
                            let key = (i + t * 31 + round * 7) % KEYS;
                            if cache.get(&overlay(key), sig).is_none() {
                                cache.insert(overlay(key), peer(key, *sig));
                            }
                        }
                    }
                });
            }
        });

        assert!(cache.len() <= PEER_CACHE_CAPACITY);
        // Every cached record is retrievable under its own overlay with the
        // signature it was stored with (no torn or misfiled entries).
        let mut cached = 0usize;
        for i in 0..KEYS {
            if cache.get(&overlay(i), &sig).is_some() {
                cached += 1;
            }
        }
        assert_eq!(cached, cache.len());
    }
}
