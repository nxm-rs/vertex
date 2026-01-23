//! Internal underlay registry for PeerId ↔ OverlayAddress mapping.
//!
//! This module is NOT exported. It encapsulates the libp2p-specific
//! PeerId mapping that bridges the network and application layers.

use std::{collections::HashMap, num::NonZeroUsize};

use libp2p::{Multiaddr, PeerId};
use lru::LruCache;
use tracing::debug;
use vertex_primitives::OverlayAddress;
use web_time::{Duration, Instant};

/// Internal registry mapping between overlay and underlay addresses.
///
/// This is the bridge layer that translates between:
/// - Swarm layer: OverlayAddress (32-byte Swarm routing address)
/// - libp2p layer: PeerId (network connection identifier)
///
/// This struct is intentionally NOT exported from the crate.
#[derive(Debug, Default)]
pub(crate) struct UnderlayRegistry {
    /// Map from overlay address to peer ID.
    overlay_to_peer: HashMap<OverlayAddress, PeerId>,
    /// Map from peer ID to overlay address.
    peer_to_overlay: HashMap<PeerId, OverlayAddress>,
}

impl UnderlayRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a mapping between overlay and peer ID.
    ///
    /// If either address was previously registered with a different mapping,
    /// the old mapping is removed.
    pub fn register(&mut self, overlay: OverlayAddress, peer_id: PeerId) {
        // Remove any existing mappings for this overlay
        if let Some(old_peer) = self.overlay_to_peer.remove(&overlay) {
            self.peer_to_overlay.remove(&old_peer);
        }

        // Remove any existing mappings for this peer_id
        if let Some(old_overlay) = self.peer_to_overlay.remove(&peer_id) {
            self.overlay_to_peer.remove(&old_overlay);
        }

        // Insert the new mapping
        self.overlay_to_peer.insert(overlay, peer_id);
        self.peer_to_overlay.insert(peer_id, overlay);
    }

    /// Remove mapping by peer ID. Returns the overlay address if found.
    pub fn remove_by_peer(&mut self, peer_id: &PeerId) -> Option<OverlayAddress> {
        if let Some(overlay) = self.peer_to_overlay.remove(peer_id) {
            self.overlay_to_peer.remove(&overlay);
            Some(overlay)
        } else {
            None
        }
    }

    /// Remove mapping by overlay address. Returns the peer ID if found.
    pub fn remove_by_overlay(&mut self, overlay: &OverlayAddress) -> Option<PeerId> {
        if let Some(peer_id) = self.overlay_to_peer.remove(overlay) {
            self.peer_to_overlay.remove(&peer_id);
            Some(peer_id)
        } else {
            None
        }
    }

    /// Resolve overlay address to peer ID.
    pub fn resolve_peer(&self, overlay: &OverlayAddress) -> Option<PeerId> {
        self.overlay_to_peer.get(overlay).copied()
    }

    /// Resolve peer ID to overlay address.
    pub fn resolve_overlay(&self, peer_id: &PeerId) -> Option<OverlayAddress> {
        self.peer_to_overlay.get(peer_id).copied()
    }

    /// Check if an overlay address is registered.
    pub fn contains_overlay(&self, overlay: &OverlayAddress) -> bool {
        self.overlay_to_peer.contains_key(overlay)
    }

    /// Check if a peer ID is registered.
    pub fn contains_peer(&self, peer_id: &PeerId) -> bool {
        self.peer_to_overlay.contains_key(peer_id)
    }

    /// Number of registered mappings.
    pub fn len(&self) -> usize {
        debug_assert_eq!(self.overlay_to_peer.len(), self.peer_to_overlay.len());
        self.overlay_to_peer.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Default maximum number of entries in the underlay cache.
pub const DEFAULT_CACHE_MAX_SIZE: usize = 10_000;

/// Default TTL for cache entries (1 hour).
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(3600);

/// Configuration for the underlay cache.
#[derive(Debug, Clone)]
pub(crate) struct UnderlayCacheConfig {
    /// Maximum number of entries in the cache (LRU eviction when exceeded).
    pub max_size: usize,
    /// Time-to-live for cache entries.
    pub ttl: Duration,
}

impl Default for UnderlayCacheConfig {
    fn default() -> Self {
        Self {
            max_size: DEFAULT_CACHE_MAX_SIZE,
            ttl: DEFAULT_CACHE_TTL,
        }
    }
}

impl UnderlayCacheConfig {
    /// Set the maximum cache size.
    pub fn with_max_size(mut self, max_size: usize) -> Self {
        self.max_size = max_size;
        self
    }

    /// Set the TTL for cache entries.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }
}

/// A cached underlay entry with expiration time.
#[derive(Debug, Clone)]
struct CacheEntry {
    /// The cached underlay addresses.
    underlays: Vec<Multiaddr>,
    /// When this entry expires.
    expires_at: Instant,
}

impl CacheEntry {
    fn new(underlays: Vec<Multiaddr>, ttl: Duration) -> Self {
        Self {
            underlays,
            expires_at: Instant::now() + ttl,
        }
    }

    fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

/// Cache of known underlay addresses (Multiaddr) for overlay addresses.
///
/// This stores the network addresses we can use to dial a peer
/// when we only know their overlay address.
///
/// Features:
/// - LRU eviction when max size is exceeded (automatic via `lru` crate)
/// - TTL-based expiration of stale entries
/// - WASM compatible (uses `web-time` for cross-platform `Instant`)
pub(crate) struct UnderlayCache {
    /// LRU cache from overlay address to cache entry.
    cache: LruCache<OverlayAddress, CacheEntry>,
    /// TTL for new entries.
    ttl: Duration,
}

impl std::fmt::Debug for UnderlayCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnderlayCache")
            .field("len", &self.cache.len())
            .field("cap", &self.cache.cap())
            .field("ttl", &self.ttl)
            .finish()
    }
}

impl Default for UnderlayCache {
    fn default() -> Self {
        Self::new(UnderlayCacheConfig::default())
    }
}

impl UnderlayCache {
    /// Create a new cache with the given configuration.
    pub fn new(config: UnderlayCacheConfig) -> Self {
        let cap = NonZeroUsize::new(config.max_size).unwrap_or(NonZeroUsize::MIN);
        Self {
            cache: LruCache::new(cap),
            ttl: config.ttl,
        }
    }

    /// Cache underlays for an overlay address.
    ///
    /// If underlays already exist for this overlay, they are replaced.
    /// LRU eviction happens automatically when capacity is exceeded.
    pub fn insert(&mut self, overlay: OverlayAddress, underlays: Vec<Multiaddr>) {
        self.cache
            .put(overlay, CacheEntry::new(underlays, self.ttl));
    }

    /// Get cached underlays for an overlay address.
    ///
    /// Returns None if the entry doesn't exist or has expired.
    /// Updates LRU order (promotes to most recently used).
    pub fn get(&mut self, overlay: &OverlayAddress) -> Option<&Vec<Multiaddr>> {
        // First check if entry exists and get it (updates LRU order)
        let entry = self.cache.get(overlay)?;

        if entry.is_expired() {
            // Remove expired entry
            self.cache.pop(overlay);
            return None;
        }

        // Re-get to return the reference (we can't return from the first get
        // because we might have removed it)
        self.cache.get(overlay).map(|e| &e.underlays)
    }

    /// Get cached underlays without updating LRU order (for batch filtering).
    ///
    /// This is used by `filter_dialable_candidates` where we need to check
    /// multiple entries efficiently without modifying cache order.
    pub fn peek(&self, overlay: &OverlayAddress) -> Option<&Vec<Multiaddr>> {
        let entry = self.cache.peek(overlay)?;
        if entry.is_expired() {
            return None;
        }
        Some(&entry.underlays)
    }

    /// Remove cached underlays for an overlay address.
    pub fn remove(&mut self, overlay: &OverlayAddress) -> Option<Vec<Multiaddr>> {
        self.cache.pop(overlay).map(|e| e.underlays)
    }

    /// Check if we have valid (non-expired) cached underlays for an overlay.
    pub fn contains(&self, overlay: &OverlayAddress) -> bool {
        self.cache
            .peek(overlay)
            .map(|e| !e.is_expired())
            .unwrap_or(false)
    }

    /// Number of cached entries (may include expired entries).
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Remove all expired entries from the cache.
    ///
    /// This is useful to call periodically to free memory from expired entries.
    /// LRU eviction handles capacity, but expired entries stay until accessed.
    pub fn evict_expired(&mut self) {
        // Collect keys of expired entries
        let expired: Vec<_> = self
            .cache
            .iter()
            .filter(|(_, entry)| entry.is_expired())
            .map(|(key, _)| *key)
            .collect();

        let count = expired.len();
        for key in expired {
            self.cache.pop(&key);
        }

        if count > 0 {
            debug!(
                count,
                remaining = self.cache.len(),
                "evicted expired cache entries"
            );
        }
    }

    /// Get all non-expired entries for persistence.
    ///
    /// Returns overlay addresses and their underlays.
    pub fn entries_for_persistence(&self) -> Vec<(OverlayAddress, Vec<Multiaddr>)> {
        self.cache
            .iter()
            .filter(|(_, entry)| !entry.is_expired())
            .map(|(overlay, entry)| (*overlay, entry.underlays.clone()))
            .collect()
    }

    /// Load entries from persistence.
    ///
    /// Entries are added with fresh TTL timestamps.
    pub fn load_from_persistence(
        &mut self,
        entries: impl IntoIterator<Item = (OverlayAddress, Vec<Multiaddr>)>,
    ) {
        for (overlay, underlays) in entries {
            if !underlays.is_empty() {
                self.insert(overlay, underlays);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    fn test_overlay(n: u8) -> OverlayAddress {
        OverlayAddress::from(B256::repeat_byte(n))
    }

    fn test_peer_id(n: u8) -> PeerId {
        // Create a deterministic peer ID from a byte
        let bytes = [n; 32];
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes).unwrap();
        let keypair =
            libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(key));
        keypair.public().to_peer_id()
    }

    #[test]
    fn test_registry_basic() {
        let mut registry = UnderlayRegistry::new();
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);

        assert!(registry.is_empty());

        registry.register(overlay, peer_id);
        assert_eq!(registry.len(), 1);
        assert!(registry.contains_overlay(&overlay));
        assert!(registry.contains_peer(&peer_id));

        assert_eq!(registry.resolve_peer(&overlay), Some(peer_id));
        assert_eq!(registry.resolve_overlay(&peer_id), Some(overlay));
    }

    #[test]
    fn test_registry_overwrite() {
        let mut registry = UnderlayRegistry::new();

        let overlay1 = test_overlay(1);
        let overlay2 = test_overlay(2);
        let peer1 = test_peer_id(1);
        let peer2 = test_peer_id(2);

        // Register first mapping
        registry.register(overlay1, peer1);
        assert_eq!(registry.len(), 1);

        // Overwrite overlay1 with new peer
        registry.register(overlay1, peer2);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.resolve_peer(&overlay1), Some(peer2));
        assert!(!registry.contains_peer(&peer1));

        // Register new overlay with peer2 (should update)
        registry.register(overlay2, peer2);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.resolve_peer(&overlay2), Some(peer2));
        assert!(!registry.contains_overlay(&overlay1));
    }

    #[test]
    fn test_registry_remove() {
        let mut registry = UnderlayRegistry::new();
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);

        registry.register(overlay, peer_id);

        let removed = registry.remove_by_peer(&peer_id);
        assert_eq!(removed, Some(overlay));
        assert!(registry.is_empty());
    }

    fn test_multiaddr(n: u8) -> Multiaddr {
        format!("/ip4/127.0.0.{}/tcp/1634", n).parse().unwrap()
    }

    #[test]
    fn test_cache_basic() {
        let config = UnderlayCacheConfig::default();
        let mut cache = UnderlayCache::new(config);

        let overlay = test_overlay(1);
        let addrs = vec![test_multiaddr(1)];

        assert!(cache.is_empty());

        cache.insert(overlay, addrs.clone());
        assert_eq!(cache.len(), 1);
        assert!(cache.contains(&overlay));
        assert_eq!(cache.get(&overlay), Some(&addrs));
    }

    #[test]
    fn test_cache_lru_eviction() {
        // Small cache that only holds 3 entries
        let config = UnderlayCacheConfig::default().with_max_size(3);
        let mut cache = UnderlayCache::new(config);

        // Insert 3 entries
        for i in 1..=3 {
            cache.insert(test_overlay(i), vec![test_multiaddr(i)]);
        }
        assert_eq!(cache.len(), 3);

        // Access overlay 1 to make it recently used
        let _ = cache.get(&test_overlay(1));

        // Insert a 4th entry - should evict overlay 2 (LRU)
        cache.insert(test_overlay(4), vec![test_multiaddr(4)]);
        assert_eq!(cache.len(), 3);

        // Overlay 1 should still exist (was accessed)
        assert!(cache.contains(&test_overlay(1)));
        // Overlay 2 should be evicted (LRU)
        assert!(!cache.contains(&test_overlay(2)));
        // Overlay 3 and 4 should exist
        assert!(cache.contains(&test_overlay(3)));
        assert!(cache.contains(&test_overlay(4)));
    }

    #[test]
    fn test_cache_ttl_expiration() {
        // Very short TTL for testing
        let config = UnderlayCacheConfig::default().with_ttl(Duration::from_millis(10));
        let mut cache = UnderlayCache::new(config);

        let overlay = test_overlay(1);
        cache.insert(overlay, vec![test_multiaddr(1)]);

        // Should be accessible immediately
        assert!(cache.contains(&overlay));

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(20));

        // Should be expired now
        assert!(!cache.contains(&overlay));
        assert!(cache.get(&overlay).is_none());
    }

    #[test]
    fn test_cache_peek_vs_get() {
        let config = UnderlayCacheConfig::default().with_max_size(2);
        let mut cache = UnderlayCache::new(config);

        // Insert 2 entries
        cache.insert(test_overlay(1), vec![test_multiaddr(1)]);
        cache.insert(test_overlay(2), vec![test_multiaddr(2)]);

        // Peek at overlay 1 (doesn't update LRU order)
        let _ = cache.peek(&test_overlay(1));

        // Insert a 3rd entry - should evict overlay 1 (still LRU because peek doesn't promote)
        cache.insert(test_overlay(3), vec![test_multiaddr(3)]);

        // Overlay 1 should be evicted
        assert!(!cache.contains(&test_overlay(1)));
        assert!(cache.contains(&test_overlay(2)));
        assert!(cache.contains(&test_overlay(3)));
    }

    #[test]
    fn test_cache_persistence_roundtrip() {
        let config = UnderlayCacheConfig::default();
        let mut cache = UnderlayCache::new(config.clone());

        // Insert some entries
        for i in 1..=5 {
            cache.insert(test_overlay(i), vec![test_multiaddr(i)]);
        }

        // Get entries for persistence
        let entries = cache.entries_for_persistence();
        assert_eq!(entries.len(), 5);

        // Create new cache and load
        let mut cache2 = UnderlayCache::new(config);
        cache2.load_from_persistence(entries);

        // Verify all entries loaded
        assert_eq!(cache2.len(), 5);
        for i in 1..=5 {
            assert!(cache2.contains(&test_overlay(i)));
        }
    }

    #[test]
    fn test_cache_evict_expired() {
        let config = UnderlayCacheConfig::default().with_ttl(Duration::from_millis(10));
        let mut cache = UnderlayCache::new(config);

        // Insert entries
        for i in 1..=5 {
            cache.insert(test_overlay(i), vec![test_multiaddr(i)]);
        }
        assert_eq!(cache.len(), 5);

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(20));

        // Entries still in cache (just expired)
        assert_eq!(cache.len(), 5);

        // Evict expired entries
        cache.evict_expired();

        // Cache should be empty now
        assert!(cache.is_empty());
    }
}
