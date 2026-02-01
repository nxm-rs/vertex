//! Smart multiaddr management for address selection and NAT discovery.
//!
//! The [`AddressManager`] handles:
//! - Tracking listen addresses from libp2p
//! - Managing configured NAT/external addresses
//! - Learning external addresses from peer observations
//! - Selecting appropriate addresses based on peer scope

use std::collections::HashMap;
use std::net::IpAddr;
use std::num::NonZeroUsize;

use libp2p::Multiaddr;
use lru::LruCache;
use parking_lot::{Mutex, RwLock};
use tracing::{debug, info, trace};
use web_time::Instant;

use crate::ip_addr::{AddressScope, classify_multiaddr, extract_ip, same_subnet};

/// Default number of peer confirmations required before trusting an observed address.
/// Requires 2 confirmations from different IP addresses (same protocol family) to
/// prevent a single malicious peer from confirming a false address.
const DEFAULT_CONFIRMATION_THRESHOLD: usize = 2;

/// Maximum number of observed addresses to track (pending confirmation).
/// Limits memory usage from adversarial peers sending many different observed addresses.
const MAX_OBSERVED_ADDRS: usize = 10;

/// Maximum number of confirmed addresses to cache.
const MAX_CONFIRMED_CACHE: usize = 20;

/// TTL for confirmed address cache entries (1 hour).
/// Peer lists are relatively stable, so a long TTL is appropriate.
const CONFIRMED_CACHE_TTL_SECS: u64 = 3600;

/// Entry tracking an address observed by peers (pending confirmation).
///
/// Storage is bounded: we only store the first confirming IP for comparison.
/// Once threshold is reached, the entry moves to the confirmed cache.
#[derive(Debug, Clone)]
struct ObservedEntry {
    /// Number of unique IPs that reported this address (1 or 2+).
    confirmations: usize,
    /// First confirming IP - stored for duplicate detection until threshold is reached.
    first_ip: IpAddr,
    /// When this address was most recently observed.
    last_seen: Instant,
}

/// Entry in the confirmed address cache.
///
/// Once an address reaches the confirmation threshold, it's moved here
/// with a TTL. Subsequent observations use this as a fast path.
/// LRU eviction is handled by the LruCache itself.
#[derive(Debug, Clone)]
struct ConfirmedEntry {
    /// When this address was confirmed (for TTL expiry).
    confirmed_at: Instant,
}

/// Manages multiaddr selection for handshake and peer advertisement.
///
/// # Address Types
///
/// - **Listen addresses**: Addresses we're actually listening on (from libp2p)
/// - **NAT addresses**: Configured external addresses for NAT traversal
/// - **Observed addresses**: Addresses peers report seeing us at
///
/// # Selection Logic
///
/// When selecting addresses for a peer based on their connection address:
///
/// - **Loopback peer**: Include loopback + private addresses
/// - **Private peer**: Include private addresses on same subnet + NAT addresses
/// - **Public peer**: Include only public addresses (NAT + confirmed observed)
pub struct AddressManager {
    /// Addresses we're listening on (updated from libp2p events).
    listen_addrs: RwLock<Vec<Multiaddr>>,

    /// Configured NAT/external addresses.
    nat_addrs: Vec<Multiaddr>,

    /// Observed addresses pending confirmation.
    observed_addrs: RwLock<HashMap<Multiaddr, ObservedEntry>>,

    /// Confirmed address LRU cache with TTL.
    /// Once an address reaches confirmation threshold, it moves here.
    /// Uses Mutex because LruCache::get mutates internal ordering.
    confirmed_cache: Mutex<LruCache<Multiaddr, ConfirmedEntry>>,

    /// Whether auto-NAT from observed addresses is enabled.
    nat_auto: bool,

    /// Number of confirmations required for observed addresses.
    confirmation_threshold: usize,

    /// TTL for confirmed cache entries in seconds.
    confirmed_ttl_secs: u64,
}

impl AddressManager {
    /// Create a new address manager.
    ///
    /// # Arguments
    ///
    /// * `nat_addrs` - Configured external/NAT addresses
    /// * `nat_auto` - Whether to automatically learn addresses from peer observations
    pub fn new(nat_addrs: Vec<Multiaddr>, nat_auto: bool) -> Self {
        Self {
            listen_addrs: RwLock::new(Vec::new()),
            nat_addrs,
            observed_addrs: RwLock::new(HashMap::new()),
            confirmed_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(MAX_CONFIRMED_CACHE).unwrap(),
            )),
            nat_auto,
            confirmation_threshold: DEFAULT_CONFIRMATION_THRESHOLD,
            confirmed_ttl_secs: CONFIRMED_CACHE_TTL_SECS,
        }
    }

    /// Create a new address manager with custom confirmation threshold.
    pub fn with_confirmation_threshold(
        nat_addrs: Vec<Multiaddr>,
        nat_auto: bool,
        threshold: usize,
    ) -> Self {
        Self {
            listen_addrs: RwLock::new(Vec::new()),
            nat_addrs,
            observed_addrs: RwLock::new(HashMap::new()),
            confirmed_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(MAX_CONFIRMED_CACHE).unwrap(),
            )),
            nat_auto,
            confirmation_threshold: threshold,
            confirmed_ttl_secs: CONFIRMED_CACHE_TTL_SECS,
        }
    }

    /// Create a new address manager with custom TTL for confirmed cache.
    pub fn with_confirmed_ttl(
        nat_addrs: Vec<Multiaddr>,
        nat_auto: bool,
        threshold: usize,
        ttl_secs: u64,
    ) -> Self {
        Self {
            listen_addrs: RwLock::new(Vec::new()),
            nat_addrs,
            observed_addrs: RwLock::new(HashMap::new()),
            confirmed_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(MAX_CONFIRMED_CACHE).unwrap(),
            )),
            nat_auto,
            confirmation_threshold: threshold,
            confirmed_ttl_secs: ttl_secs,
        }
    }

    /// Called when libp2p reports a new listen address.
    pub fn on_new_listen_addr(&self, addr: Multiaddr) {
        let mut addrs = self.listen_addrs.write();
        if !addrs.contains(&addr) {
            debug!(listen_addr = %addr, "new listen address");
            addrs.push(addr);
        }
    }

    /// Called when libp2p reports an expired listen address.
    pub fn on_expired_listen_addr(&self, addr: &Multiaddr) {
        let mut addrs = self.listen_addrs.write();
        if let Some(pos) = addrs.iter().position(|a| a == addr) {
            debug!(listen_addr = %addr, "expired listen address");
            addrs.remove(pos);
        }
    }

    /// Called when a peer reports our observed address during handshake.
    ///
    /// The `from_peer` address is used to validate that public peers
    /// are reporting public addresses (prevents private peers from
    /// claiming we're at a public address).
    ///
    /// Confirmations are only counted from unique IP addresses within the same
    /// protocol family (IPv4 or IPv6) to prevent a single peer from confirming
    /// by reconnecting multiple times.
    ///
    /// Once an address is confirmed, it's moved to the confirmed cache with a TTL.
    /// Subsequent observations of cached addresses use a fast path that just
    /// updates the last_accessed timestamp.
    pub fn on_observed_addr(&self, addr: Multiaddr, from_peer: &Multiaddr) {
        trace!(
            observed_addr = %addr,
            peer_addr = %from_peer,
            nat_auto = self.nat_auto,
            "received observed address from peer"
        );

        if !self.nat_auto {
            trace!("nat_auto disabled, ignoring observed address");
            return;
        }

        // Extract the peer's IP for deduplication
        let peer_ip = match extract_ip(from_peer) {
            Some(ip) => ip,
            None => {
                trace!("could not extract peer IP, ignoring");
                return;
            }
        };

        // Only trust observed public addresses from public peers
        let peer_scope = classify_multiaddr(from_peer);
        let observed_scope = classify_multiaddr(&addr);

        trace!(?peer_scope, ?observed_scope, "classified scopes");

        // Ignore if we can't classify the addresses
        let (Some(peer_scope), Some(observed_scope)) = (peer_scope, observed_scope) else {
            trace!("could not classify addresses, ignoring");
            return;
        };

        // Only learn public addresses from public peers
        if observed_scope == AddressScope::Public && peer_scope != AddressScope::Public {
            trace!(
                observed_addr = %addr,
                peer_addr = %from_peer,
                "ignoring public address from non-public peer"
            );
            return;
        }

        // Check protocol family match (IPv4 observed should be confirmed by IPv4 peers)
        let observed_ip = extract_ip(&addr);
        if let Some(obs_ip) = &observed_ip {
            let same_family = matches!(
                (obs_ip, &peer_ip),
                (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
            );
            if !same_family {
                trace!(
                    observed_addr = %addr,
                    %peer_ip,
                    "ignoring confirmation from different protocol family"
                );
                return;
            }
        }

        let now = Instant::now();
        let ttl = std::time::Duration::from_secs(self.confirmed_ttl_secs);

        // Fast path: check confirmed cache first
        {
            let mut cache = self.confirmed_cache.lock();
            // LruCache::get automatically marks as recently used
            if let Some(entry) = cache.get(&addr) {
                // Check if entry is still valid (not expired)
                if now.duration_since(entry.confirmed_at) < ttl {
                    trace!(
                        observed_addr = %addr,
                        "confirmed address in cache (fast path)"
                    );
                    return;
                } else {
                    // Expired - remove from cache and fall through to re-confirm
                    debug!(
                        observed_addr = %addr,
                        "confirmed address expired, removing from cache"
                    );
                    cache.pop(&addr);
                }
            }
        }

        // Normal path: track in observed_addrs until confirmed
        let mut observed = self.observed_addrs.write();

        if let Some(entry) = observed.get_mut(&addr) {
            // Already tracking this address
            entry.last_seen = now;

            // Check if this is a new IP (different from first_ip)
            if peer_ip != entry.first_ip {
                entry.confirmations += 1;
                info!(
                    observed_addr = %addr,
                    confirming_ip = %peer_ip,
                    confirmations = entry.confirmations,
                    threshold = self.confirmation_threshold,
                    "confirmed by second unique IP"
                );

                // Check if now confirmed - move to cache
                if entry.confirmations >= self.confirmation_threshold {
                    let addr_to_cache = addr.clone();
                    observed.remove(&addr);
                    drop(observed); // Release lock before acquiring cache lock
                    self.add_to_confirmed_cache(addr_to_cache, now);
                }
            } else {
                trace!(
                    observed_addr = %addr,
                    %peer_ip,
                    "duplicate confirmation from same IP, ignoring"
                );
            }
        } else {
            // First observation of this address - check capacity
            if observed.len() >= MAX_OBSERVED_ADDRS {
                // At capacity - try to evict an unconfirmed entry (oldest first)
                let evict_candidate = observed
                    .iter()
                    .min_by_key(|(_, e)| e.last_seen)
                    .map(|(a, _)| a.clone());

                if let Some(evicted_addr) = evict_candidate {
                    debug!(
                        %evicted_addr,
                        "evicting oldest observed address to make room"
                    );
                    observed.remove(&evicted_addr);
                }
            }

            // Check if single confirmation is enough (threshold=1)
            if self.confirmation_threshold <= 1 {
                drop(observed); // Release lock before acquiring cache lock
                self.add_to_confirmed_cache(addr, now);
            } else {
                debug!(
                    observed_addr = %addr,
                    first_ip = %peer_ip,
                    threshold = self.confirmation_threshold,
                    "new observed address pending (1/{} confirmations)",
                    self.confirmation_threshold
                );
                observed.insert(
                    addr,
                    ObservedEntry {
                        confirmations: 1,
                        first_ip: peer_ip,
                        last_seen: now,
                    },
                );
            }
        }
    }

    /// Add an address to the confirmed cache.
    ///
    /// LruCache handles capacity and LRU eviction automatically.
    fn add_to_confirmed_cache(&self, addr: Multiaddr, now: Instant) {
        let mut cache = self.confirmed_cache.lock();

        info!(
            observed_addr = %addr,
            ttl_secs = self.confirmed_ttl_secs,
            cache_size = cache.len(),
            cache_cap = cache.cap(),
            "address added to confirmed cache"
        );

        // LruCache::put automatically evicts LRU entry if at capacity
        cache.put(addr, ConfirmedEntry { confirmed_at: now });
    }

    /// Get addresses to advertise for a peer based on their connection scope.
    ///
    /// This implements smart address selection:
    /// - Loopback peers see loopback + private addresses
    /// - Private peers see same-subnet private addresses + NAT addresses
    /// - Public peers see only public addresses
    pub fn addresses_for_peer(&self, peer_addr: &Multiaddr) -> Vec<Multiaddr> {
        let peer_scope = classify_multiaddr(peer_addr).unwrap_or(AddressScope::Public);
        let listen = self.listen_addrs.read();
        let cache = self.confirmed_cache.lock();

        trace!(
            peer_addr = %peer_addr,
            ?peer_scope,
            listen_count = listen.len(),
            nat_count = self.nat_addrs.len(),
            confirmed_cache_count = cache.len(),
            nat_auto = self.nat_auto,
            "selecting addresses for peer"
        );

        let now = Instant::now();
        let ttl = std::time::Duration::from_secs(self.confirmed_ttl_secs);

        let mut result = Vec::new();

        match peer_scope {
            AddressScope::Loopback => {
                // Include loopback and private listen addresses
                for addr in listen.iter() {
                    if let Some(AddressScope::Loopback | AddressScope::Private) = classify_multiaddr(addr) {
                        result.push(addr.clone());
                    }
                }
            }
            AddressScope::Private | AddressScope::LinkLocal => {
                // Include private listen addresses on same subnet
                for addr in listen.iter() {
                    if same_subnet(addr, peer_addr) {
                        result.push(addr.clone());
                    }
                }
                // Also include NAT addresses (for reaching us from elsewhere)
                result.extend(self.nat_addrs.iter().cloned());
            }
            AddressScope::Public => {
                // Include only public listen addresses
                for addr in listen.iter() {
                    if classify_multiaddr(addr) == Some(AddressScope::Public) {
                        result.push(addr.clone());
                    }
                }
                // Include NAT addresses
                result.extend(self.nat_addrs.iter().cloned());
                // Include confirmed observed addresses from cache
                if self.nat_auto {
                    // LruCache::iter doesn't modify order (use peek_iter for read-only)
                    for (addr, entry) in cache.iter() {
                        // Skip expired entries
                        if now.duration_since(entry.confirmed_at) >= ttl {
                            continue;
                        }
                        if classify_multiaddr(addr) == Some(AddressScope::Public)
                            && !result.contains(addr) {
                                result.push(addr.clone());
                            }
                    }
                }
            }
        }

        // Deduplicate while preserving order
        let mut seen = Vec::new();
        result.retain(|addr| {
            if seen.contains(addr) {
                false
            } else {
                seen.push(addr.clone());
                true
            }
        });

        trace!(
            count = result.len(),
            addrs = ?result,
            "selected addresses"
        );

        result
    }

    /// Get all known addresses (for non-selective use cases).
    ///
    /// Returns listen addresses + NAT addresses + confirmed observed addresses.
    pub fn all_addresses(&self) -> Vec<Multiaddr> {
        let listen = self.listen_addrs.read();
        let cache = self.confirmed_cache.lock();

        let now = Instant::now();
        let ttl = std::time::Duration::from_secs(self.confirmed_ttl_secs);

        let mut result: Vec<Multiaddr> = listen.iter().cloned().collect();

        // Add NAT addresses
        for addr in &self.nat_addrs {
            if !result.contains(addr) {
                result.push(addr.clone());
            }
        }

        // Add confirmed observed addresses from cache (non-expired)
        if self.nat_auto {
            for (addr, entry) in cache.iter() {
                if now.duration_since(entry.confirmed_at) < ttl && !result.contains(addr) {
                    result.push(addr.clone());
                }
            }
        }

        result
    }

    /// Get the current listen addresses.
    pub fn listen_addrs(&self) -> Vec<Multiaddr> {
        self.listen_addrs.read().clone()
    }

    /// Get the configured NAT addresses.
    pub fn nat_addrs(&self) -> &[Multiaddr] {
        &self.nat_addrs
    }

    /// Check if auto-NAT is enabled.
    pub fn nat_auto_enabled(&self) -> bool {
        self.nat_auto
    }

    /// Get the number of pending observed addresses (for diagnostics).
    pub fn pending_observed_count(&self) -> usize {
        self.observed_addrs.read().len()
    }

    /// Get the number of confirmed cached addresses (for diagnostics).
    pub fn confirmed_cache_count(&self) -> usize {
        self.confirmed_cache.lock().len()
    }

    /// Get confirmed observed addresses from cache (for diagnostics).
    ///
    /// Only returns non-expired entries.
    pub fn confirmed_observed_addrs(&self) -> Vec<Multiaddr> {
        let cache = self.confirmed_cache.lock();
        let now = Instant::now();
        let ttl = std::time::Duration::from_secs(self.confirmed_ttl_secs);

        cache
            .iter()
            .filter(|(_, entry): &(&Multiaddr, &ConfirmedEntry)| {
                now.duration_since(entry.confirmed_at) < ttl
            })
            .map(|(addr, _)| addr.clone())
            .collect()
    }

    /// Get the configured TTL for confirmed addresses in seconds.
    pub fn confirmed_ttl_secs(&self) -> u64 {
        self.confirmed_ttl_secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_addr(s: &str) -> Multiaddr {
        s.parse().unwrap()
    }

    #[test]
    fn test_new_address_manager() {
        let nat_addrs = vec![parse_addr("/ip4/203.0.113.50/tcp/1634")];
        let mgr = AddressManager::new(nat_addrs.clone(), true);

        assert_eq!(mgr.nat_addrs(), nat_addrs.as_slice());
        assert!(mgr.nat_auto_enabled());
        assert!(mgr.listen_addrs().is_empty());
    }

    #[test]
    fn test_listen_addr_tracking() {
        let mgr = AddressManager::new(vec![], false);

        let addr1 = parse_addr("/ip4/0.0.0.0/tcp/1634");
        let addr2 = parse_addr("/ip6/::/tcp/1634");

        mgr.on_new_listen_addr(addr1.clone());
        mgr.on_new_listen_addr(addr2.clone());

        let addrs = mgr.listen_addrs();
        assert_eq!(addrs.len(), 2);
        assert!(addrs.contains(&addr1));
        assert!(addrs.contains(&addr2));

        mgr.on_expired_listen_addr(&addr1);
        let addrs = mgr.listen_addrs();
        assert_eq!(addrs.len(), 1);
        assert!(!addrs.contains(&addr1));
        assert!(addrs.contains(&addr2));
    }

    #[test]
    fn test_observed_addr_confirmation() {
        let mgr = AddressManager::with_confirmation_threshold(vec![], true, 2);

        let observed = parse_addr("/ip4/203.0.113.100/tcp/1634");
        let peer1 = parse_addr("/ip4/8.8.8.8/tcp/5000");
        let peer2 = parse_addr("/ip4/1.1.1.1/tcp/5000");

        // First observation - not yet confirmed
        mgr.on_observed_addr(observed.clone(), &peer1);
        assert_eq!(mgr.confirmed_observed_addrs().len(), 0);

        // Second observation - now confirmed
        mgr.on_observed_addr(observed.clone(), &peer2);
        let confirmed = mgr.confirmed_observed_addrs();
        assert_eq!(confirmed.len(), 1);
        assert!(confirmed.contains(&observed));
    }

    #[test]
    fn test_addresses_for_loopback_peer() {
        let mgr = AddressManager::new(vec![], false);

        mgr.on_new_listen_addr(parse_addr("/ip4/127.0.0.1/tcp/1634"));
        mgr.on_new_listen_addr(parse_addr("/ip4/192.168.1.100/tcp/1634"));
        mgr.on_new_listen_addr(parse_addr("/ip4/8.8.8.8/tcp/1634"));

        let peer = parse_addr("/ip4/127.0.0.1/tcp/5000");
        let addrs = mgr.addresses_for_peer(&peer);

        // Loopback peer should see loopback and private
        assert!(addrs.iter().any(|a| a.to_string().contains("127.0.0.1")));
        assert!(addrs.iter().any(|a| a.to_string().contains("192.168")));
        // But not public
        assert!(!addrs.iter().any(|a| a.to_string().contains("8.8.8.8")));
    }

    #[test]
    fn test_addresses_for_private_peer() {
        // This test uses actual local network interfaces to test same-subnet logic.
        // We find a real local subnet and use IPs from it.
        let info = crate::local_network::get_local_network_info();

        // Find a private subnet from local interfaces
        let private_subnet = info.ipv4_subnets.iter().find(|s| s.addr().is_private());

        if let Some(subnet) = private_subnet {
            let hosts: Vec<std::net::Ipv4Addr> = subnet.hosts().take(3).collect();
            if hosts.len() >= 2 {
                let local_ip = hosts[0];
                let peer_ip = hosts[1];

                let nat_addr = parse_addr("/ip4/203.0.113.50/tcp/1634");
                let mgr = AddressManager::new(vec![nat_addr.clone()], false);

                // Add a listen address on the same subnet as our test peer
                let listen_addr = parse_addr(&format!("/ip4/{}/tcp/1634", local_ip));
                mgr.on_new_listen_addr(listen_addr.clone());

                // Add a listen address on a different subnet (public)
                mgr.on_new_listen_addr(parse_addr("/ip4/8.8.8.8/tcp/1634"));

                let peer = parse_addr(&format!("/ip4/{}/tcp/5000", peer_ip));
                let addrs = mgr.addresses_for_peer(&peer);

                // Private peer should see same-subnet private address
                assert!(
                    addrs.contains(&listen_addr),
                    "Expected {} in {:?}",
                    listen_addr,
                    addrs
                );
                // Should include NAT address
                assert!(addrs.contains(&nat_addr));
                // But not public listen address
                assert!(!addrs.iter().any(|a| a.to_string().contains("8.8.8.8")));
            } else {
                println!("Not enough hosts in subnet, skipping test");
            }
        } else {
            // No private subnets - skip this test (e.g., in restricted container)
            println!("No private subnets discovered, skipping test_addresses_for_private_peer");
        }
    }

    #[test]
    fn test_addresses_for_public_peer() {
        let nat_addr = parse_addr("/ip4/203.0.113.50/tcp/1634");
        let mgr = AddressManager::with_confirmation_threshold(vec![nat_addr.clone()], true, 1);

        mgr.on_new_listen_addr(parse_addr("/ip4/192.168.1.100/tcp/1634"));
        mgr.on_new_listen_addr(parse_addr("/ip4/8.8.4.4/tcp/1634"));

        // Add a confirmed observed address
        let observed = parse_addr("/ip4/198.51.100.10/tcp/1634");
        let public_peer = parse_addr("/ip4/8.8.8.8/tcp/5000");
        mgr.on_observed_addr(observed.clone(), &public_peer);

        let addrs = mgr.addresses_for_peer(&public_peer);

        // Public peer should NOT see private addresses
        assert!(!addrs.iter().any(|a| a.to_string().contains("192.168")));
        // Should see public listen address
        assert!(addrs.iter().any(|a| a.to_string().contains("8.8.4.4")));
        // Should see NAT address
        assert!(addrs.contains(&nat_addr));
        // Should see confirmed observed address
        assert!(addrs.contains(&observed));
    }

    #[test]
    fn test_ignore_observed_from_private_peer() {
        let mgr = AddressManager::with_confirmation_threshold(vec![], true, 1);

        let observed_public = parse_addr("/ip4/203.0.113.100/tcp/1634");
        let private_peer = parse_addr("/ip4/192.168.1.50/tcp/5000");

        // Private peer claims we're at a public address - should be ignored
        mgr.on_observed_addr(observed_public.clone(), &private_peer);

        assert_eq!(mgr.confirmed_observed_addrs().len(), 0);
    }

    #[test]
    fn test_all_addresses() {
        let nat_addr = parse_addr("/ip4/203.0.113.50/tcp/1634");
        let mgr = AddressManager::with_confirmation_threshold(vec![nat_addr.clone()], true, 1);

        let listen1 = parse_addr("/ip4/192.168.1.100/tcp/1634");
        let listen2 = parse_addr("/ip4/8.8.4.4/tcp/1634");
        mgr.on_new_listen_addr(listen1.clone());
        mgr.on_new_listen_addr(listen2.clone());

        // Add confirmed observed
        let observed = parse_addr("/ip4/198.51.100.10/tcp/1634");
        let public_peer = parse_addr("/ip4/8.8.8.8/tcp/5000");
        mgr.on_observed_addr(observed.clone(), &public_peer);

        let all = mgr.all_addresses();

        assert!(all.contains(&listen1));
        assert!(all.contains(&listen2));
        assert!(all.contains(&nat_addr));
        assert!(all.contains(&observed));
    }

    #[test]
    fn test_confirmed_cache_fast_path() {
        // Use threshold=2 to test the confirmation flow
        let mgr = AddressManager::with_confirmation_threshold(vec![], true, 2);

        let observed = parse_addr("/ip4/203.0.113.100/tcp/1634");
        let peer1 = parse_addr("/ip4/8.8.8.8/tcp/5000");
        let peer2 = parse_addr("/ip4/1.1.1.1/tcp/5000");
        let peer3 = parse_addr("/ip4/9.9.9.9/tcp/5000");

        // First observation - pending, not in cache
        mgr.on_observed_addr(observed.clone(), &peer1);
        assert_eq!(mgr.pending_observed_count(), 1);
        assert_eq!(mgr.confirmed_cache_count(), 0);

        // Second observation from different IP - confirmed, moved to cache
        mgr.on_observed_addr(observed.clone(), &peer2);
        assert_eq!(mgr.pending_observed_count(), 0); // Removed from pending
        assert_eq!(mgr.confirmed_cache_count(), 1); // Added to cache

        // Third observation - fast path, just updates cache timestamp
        mgr.on_observed_addr(observed.clone(), &peer3);
        assert_eq!(mgr.pending_observed_count(), 0);
        assert_eq!(mgr.confirmed_cache_count(), 1);

        // Verify in confirmed list
        let confirmed = mgr.confirmed_observed_addrs();
        assert_eq!(confirmed.len(), 1);
        assert!(confirmed.contains(&observed));
    }

    #[test]
    fn test_confirmed_cache_lru_eviction() {
        // Create manager with very small cache for testing
        let mgr = AddressManager::with_confirmation_threshold(vec![], true, 1);

        // Fill up the cache beyond MAX_CONFIRMED_CACHE (20)
        // Using threshold=1 so single confirmation adds to cache
        for i in 0..25 {
            let observed = parse_addr(&format!("/ip4/203.0.113.{}/tcp/1634", i));
            let peer = parse_addr(&format!("/ip4/8.8.8.{}/tcp/5000", i));
            mgr.on_observed_addr(observed, &peer);
        }

        // Should be capped at MAX_CONFIRMED_CACHE
        assert!(mgr.confirmed_cache_count() <= MAX_CONFIRMED_CACHE);
    }
}
