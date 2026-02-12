//! Swarm-specific NAT auto-discovery via peer-observed addresses.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::OnceLock;

use hashlink::LruCache;
use libp2p::{Multiaddr, PeerId};
use libp2p::multiaddr::Protocol;
use parking_lot::{Mutex, RwLock};
use tracing::{debug, info, warn};
use vertex_net_local::{AddressScope, LocalCapabilities, NetworkCapability, classify_multiaddr, extract_ip};
use web_time::Instant;

/// Require 2 unique IPs to confirm an observed address (prevents single-peer spoofing).
const DEFAULT_CONFIRMATION_THRESHOLD: usize = 2;
/// Max pending observed addresses (bounds memory from adversarial peers).
const MAX_OBSERVED_ADDRS: usize = 10;
const MAX_CONFIRMED_CACHE: usize = 20;
/// 1 hour TTL for confirmed addresses.
const CONFIRMED_CACHE_TTL_SECS: u64 = 3600;

#[derive(Debug, Clone)]
struct ObservedEntry {
    confirmations: usize,
    first_ip: IpAddr,
    last_seen: Instant,
}

#[derive(Debug, Clone)]
struct ConfirmedEntry {
    confirmed_at: Instant,
}

/// Configuration for NAT auto-discovery.
#[derive(Debug, Clone)]
pub struct NatDiscoveryConfig {
    pub confirmation_threshold: usize,
    pub confirmed_ttl_secs: u64,
}

impl Default for NatDiscoveryConfig {
    fn default() -> Self {
        Self {
            confirmation_threshold: DEFAULT_CONFIRMATION_THRESHOLD,
            confirmed_ttl_secs: CONFIRMED_CACHE_TTL_SECS,
        }
    }
}

/// Swarm-specific NAT discovery via peer-observed addresses.
///
/// Wraps LocalCapabilities and adds:
/// - Static NAT addresses (configured at startup)
/// - Multi-peer confirmation for peer-observed addresses
/// - Local PeerId for appending /p2p/ to advertised addresses
pub struct NatDiscovery {
    local: Arc<LocalCapabilities>,
    nat_addrs: Vec<Multiaddr>,
    observed: RwLock<HashMap<Multiaddr, ObservedEntry>>,
    /// Mutex because LruCache::get mutates internal ordering.
    confirmed: Mutex<LruCache<Multiaddr, ConfirmedEntry>>,
    config: NatDiscoveryConfig,
    enabled: bool,
    /// Local PeerId for appending /p2p/ to advertised addresses.
    /// Set via `set_local_peer_id()` after swarm is built.
    local_peer_id: OnceLock<PeerId>,
}

impl NatDiscovery {
    pub fn new(
        local: Arc<LocalCapabilities>,
        nat_addrs: Vec<Multiaddr>,
        config: NatDiscoveryConfig,
        enabled: bool,
    ) -> Self {
        Self {
            local,
            nat_addrs,
            observed: RwLock::new(HashMap::new()),
            confirmed: Mutex::new(LruCache::new(MAX_CONFIRMED_CACHE)),
            config,
            enabled,
            local_peer_id: OnceLock::new(),
        }
    }

    /// Set the local PeerId for appending /p2p/ to advertised addresses.
    ///
    /// Must be called after the libp2p Swarm is built. Can only be set once.
    /// Addresses returned by `addresses_for_peer()` will include /p2p/{peer_id}.
    pub fn set_local_peer_id(&self, peer_id: PeerId) {
        if self.local_peer_id.set(peer_id).is_err() {
            warn!("local_peer_id already set, ignoring duplicate call");
        } else {
            debug!(%peer_id, "Local PeerId set for address advertisement");
        }
    }

    /// Get the local PeerId if set.
    pub fn local_peer_id(&self) -> Option<&PeerId> {
        self.local_peer_id.get()
    }

    pub fn disabled(local: Arc<LocalCapabilities>) -> Self {
        Self {
            local,
            nat_addrs: vec![],
            observed: RwLock::new(HashMap::new()),
            confirmed: Mutex::new(LruCache::new(MAX_CONFIRMED_CACHE)),
            config: NatDiscoveryConfig::default(),
            enabled: false,
            local_peer_id: OnceLock::new(),
        }
    }

    pub fn local_capabilities(&self) -> &Arc<LocalCapabilities> {
        &self.local
    }

    pub fn nat_addrs(&self) -> &[Multiaddr] {
        &self.nat_addrs
    }

    pub fn capability(&self) -> NetworkCapability {
        self.local.capability()
    }

    /// Record observed address from peer (requires multi-peer confirmation).
    ///
    /// Lock duration bounded by MAX_OBSERVED_ADDRS (10 entries max).
    pub fn on_observed_addr(&self, addr: Multiaddr, from_peer: &Multiaddr) {
        if !self.enabled {
            return;
        }

        let Some(peer_ip) = extract_ip(from_peer) else {
            return;
        };

        let peer_scope = classify_multiaddr(from_peer);
        let observed_scope = classify_multiaddr(&addr);

        let (Some(peer_scope), Some(observed_scope)) = (peer_scope, observed_scope) else {
            return;
        };

        // Only learn public addresses from public peers
        if observed_scope == AddressScope::Public && peer_scope != AddressScope::Public {
            return;
        }

        // Protocol family must match
        if let Some(obs_ip) = extract_ip(&addr) {
            let same_family = matches!(
                (&obs_ip, &peer_ip),
                (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
            );
            if !same_family {
                return;
            }
        }

        let now = Instant::now();
        let ttl = std::time::Duration::from_secs(self.config.confirmed_ttl_secs);

        // Fast path: already confirmed
        {
            let mut cache = self.confirmed.lock();
            if let Some(entry) = cache.get(&addr) {
                if now.duration_since(entry.confirmed_at) < ttl {
                    return;
                }
                cache.remove(&addr);
            }
        }

        // Track in observed until confirmed
        let mut observed = self.observed.write();

        if let Some(entry) = observed.get_mut(&addr) {
            entry.last_seen = now;

            if peer_ip == entry.first_ip {
                return; // Same IP, no new confirmation
            }

            entry.confirmations += 1;
            info!(
                observed_addr = %addr,
                confirming_ip = %peer_ip,
                confirmations = entry.confirmations,
                "address confirmed by second unique IP"
            );

            if entry.confirmations >= self.config.confirmation_threshold {
                let addr_to_cache = addr.clone();
                observed.remove(&addr);
                drop(observed);
                self.add_to_confirmed_cache(addr_to_cache, now);
            }
            return;
        }

        // First observation
        if self.config.confirmation_threshold <= 1 {
            drop(observed);
            self.add_to_confirmed_cache(addr, now);
            return;
        }

        // Evict oldest if at capacity
        if observed.len() >= MAX_OBSERVED_ADDRS {
            if let Some(evict) = observed.iter().min_by_key(|(_, e)| e.last_seen).map(|(a, _)| a.clone()) {
                debug!(%evict, "evicting oldest observed address");
                observed.remove(&evict);
            }
        }

        debug!(
            observed_addr = %addr,
            first_ip = %peer_ip,
            threshold = self.config.confirmation_threshold,
            "new observed address pending"
        );

        observed.insert(addr, ObservedEntry {
            confirmations: 1,
            first_ip: peer_ip,
            last_seen: now,
        });
    }

    fn add_to_confirmed_cache(&self, addr: Multiaddr, now: Instant) {
        let mut cache = self.confirmed.lock();

        info!(
            observed_addr = %addr,
            ttl_secs = self.config.confirmed_ttl_secs,
            cache_size = cache.len(),
            cache_cap = cache.capacity(),
            "address added to confirmed cache"
        );

        cache.insert(addr, ConfirmedEntry { confirmed_at: now });
    }

    /// Select addresses to advertise to peer during handshake.
    ///
    /// All returned addresses include `/p2p/{local_peer_id}` if the local
    /// PeerId has been set via `set_local_peer_id()`.
    pub fn addresses_for_peer(&self, peer_addr: &Multiaddr) -> Vec<Multiaddr> {
        use std::collections::HashSet;

        let peer_scope = classify_multiaddr(peer_addr).unwrap_or(AddressScope::Public);

        // Start with addresses from local capabilities
        let local_addrs = self.local.addresses_for_scope(peer_scope, Some(peer_addr));

        // NAT addresses for non-loopback peers
        let nat_addrs = self
            .nat_addrs
            .iter()
            .filter(|_| peer_scope != AddressScope::Loopback)
            .cloned();

        // Confirmed observed addresses for public peers
        let ttl = std::time::Duration::from_secs(self.config.confirmed_ttl_secs);
        let now = Instant::now();
        let confirmed_addrs = if self.enabled && peer_scope == AddressScope::Public {
            let cache = self.confirmed.lock();
            cache
                .iter()
                .filter(|(addr, entry)| {
                    now.duration_since(entry.confirmed_at) < ttl
                        && classify_multiaddr(addr) == Some(AddressScope::Public)
                })
                .map(|(addr, _)| addr.clone())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        // Chain all sources and deduplicate
        let mut seen = HashSet::new();
        let addrs: Vec<Multiaddr> = local_addrs
            .into_iter()
            .chain(nat_addrs)
            .chain(confirmed_addrs)
            .filter(|addr| seen.insert(addr.clone()))
            .collect();

        // Append /p2p/{local_peer_id} to all addresses
        self.with_peer_id(addrs)
    }

    /// Append /p2p/{local_peer_id} to addresses if local PeerId is set.
    fn with_peer_id(&self, addrs: Vec<Multiaddr>) -> Vec<Multiaddr> {
        let Some(peer_id) = self.local_peer_id.get() else {
            warn!("local_peer_id not set, returning addresses without /p2p/");
            return addrs;
        };

        addrs
            .into_iter()
            .map(|addr| addr.with(Protocol::P2p(*peer_id)))
            .collect()
    }

    /// All known addresses (listen + NAT + confirmed observed).
    pub fn all_addresses(&self) -> Vec<Multiaddr> {
        use std::collections::HashSet;

        let listen_addrs = self.local.listen_addrs();
        let nat_addrs = self.nat_addrs.iter().cloned();

        let confirmed_addrs = if self.enabled {
            let ttl = std::time::Duration::from_secs(self.config.confirmed_ttl_secs);
            let now = Instant::now();
            let cache = self.confirmed.lock();
            cache
                .iter()
                .filter(|(_, entry)| now.duration_since(entry.confirmed_at) < ttl)
                .map(|(addr, _)| addr.clone())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        // Deduplicate
        let mut seen = HashSet::new();
        listen_addrs
            .into_iter()
            .chain(nat_addrs)
            .chain(confirmed_addrs.into_iter())
            .filter(|addr| seen.insert(addr.clone()))
            .collect()
    }

    // Delegate to local capabilities
    pub fn filter_dialable<'a>(
        &self,
        addrs: &'a [Multiaddr],
    ) -> impl Iterator<Item = &'a Multiaddr> {
        self.local.filter_dialable(addrs)
    }

    /// Returns `true` if this address caused capability to become known.
    pub fn on_new_listen_addr(&self, addr: Multiaddr) -> bool {
        self.local.on_new_listen_addr(addr)
    }

    pub fn on_expired_listen_addr(&self, addr: &Multiaddr) {
        self.local.on_expired_listen_addr(addr);
    }

    pub fn listen_addrs(&self) -> Vec<Multiaddr> {
        self.local.listen_addrs()
    }

    // Diagnostic methods
    pub fn pending_observed_count(&self) -> usize {
        self.observed.read().len()
    }

    pub fn confirmed_cache_count(&self) -> usize {
        self.confirmed.lock().len()
    }

    pub fn nat_auto_enabled(&self) -> bool {
        self.enabled
    }

    pub fn confirmed_observed_addrs(&self) -> Vec<Multiaddr> {
        let cache = self.confirmed.lock();
        let now = Instant::now();
        let ttl = std::time::Duration::from_secs(self.config.confirmed_ttl_secs);

        cache
            .iter()
            .filter(|(_, entry)| now.duration_since(entry.confirmed_at) < ttl)
            .map(|(addr, _)| addr.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_addr(s: &str) -> Multiaddr {
        s.parse().unwrap()
    }

    fn create_discovery(nat_auto: bool, threshold: usize) -> NatDiscovery {
        let local = Arc::new(LocalCapabilities::new());
        let config = NatDiscoveryConfig {
            confirmation_threshold: threshold,
            ..Default::default()
        };
        NatDiscovery::new(local, vec![], config, nat_auto)
    }

    #[test]
    fn test_disabled_ignores_observed() {
        let discovery = create_discovery(false, 2);

        let observed = parse_addr("/ip4/203.0.113.100/tcp/1634");
        let peer = parse_addr("/ip4/8.8.8.8/tcp/5000");

        discovery.on_observed_addr(observed.clone(), &peer);

        assert_eq!(discovery.pending_observed_count(), 0);
        assert_eq!(discovery.confirmed_cache_count(), 0);
    }

    #[test]
    fn test_observed_addr_confirmation() {
        let discovery = create_discovery(true, 2);

        let observed = parse_addr("/ip4/203.0.113.100/tcp/1634");
        let peer1 = parse_addr("/ip4/8.8.8.8/tcp/5000");
        let peer2 = parse_addr("/ip4/1.1.1.1/tcp/5000");

        // First observation - not yet confirmed
        discovery.on_observed_addr(observed.clone(), &peer1);
        assert_eq!(discovery.pending_observed_count(), 1);
        assert_eq!(discovery.confirmed_cache_count(), 0);

        // Second observation from different IP - now confirmed
        discovery.on_observed_addr(observed.clone(), &peer2);
        assert_eq!(discovery.pending_observed_count(), 0);
        assert_eq!(discovery.confirmed_cache_count(), 1);

        let confirmed = discovery.confirmed_observed_addrs();
        assert_eq!(confirmed.len(), 1);
        assert!(confirmed.contains(&observed));
    }

    #[test]
    fn test_threshold_one_immediate_confirmation() {
        let discovery = create_discovery(true, 1);

        let observed = parse_addr("/ip4/203.0.113.100/tcp/1634");
        let peer = parse_addr("/ip4/8.8.8.8/tcp/5000");

        discovery.on_observed_addr(observed.clone(), &peer);

        // With threshold=1, single confirmation is enough
        assert_eq!(discovery.pending_observed_count(), 0);
        assert_eq!(discovery.confirmed_cache_count(), 1);
    }

    #[test]
    fn test_ignore_public_from_private_peer() {
        let discovery = create_discovery(true, 1);

        let observed = parse_addr("/ip4/203.0.113.100/tcp/1634");
        let private_peer = parse_addr("/ip4/192.168.1.50/tcp/5000");

        discovery.on_observed_addr(observed.clone(), &private_peer);

        // Should be ignored - private peer claiming public address
        assert_eq!(discovery.confirmed_cache_count(), 0);
    }

    #[test]
    fn test_addresses_for_peer_includes_nat_and_confirmed() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/8.8.4.4/tcp/1634"));

        let nat_addr = parse_addr("/ip4/203.0.113.50/tcp/1634");
        let config = NatDiscoveryConfig {
            confirmation_threshold: 1,
            ..Default::default()
        };
        let discovery = NatDiscovery::new(local, vec![nat_addr.clone()], config, true);

        // Add confirmed observed address
        let observed = parse_addr("/ip4/198.51.100.10/tcp/1634");
        let public_peer = parse_addr("/ip4/8.8.8.8/tcp/5000");
        discovery.on_observed_addr(observed.clone(), &public_peer);

        let addrs = discovery.addresses_for_peer(&public_peer);

        // Should include listen address, NAT address, and confirmed observed
        assert!(addrs.iter().any(|a| a.to_string().contains("8.8.4.4")));
        assert!(addrs.contains(&nat_addr));
        assert!(addrs.contains(&observed));
    }

    #[test]
    fn test_nat_addrs_not_for_loopback() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/127.0.0.1/tcp/1634"));

        let nat_addr = parse_addr("/ip4/203.0.113.50/tcp/1634");
        let discovery = NatDiscovery::new(
            local,
            vec![nat_addr.clone()],
            NatDiscoveryConfig::default(),
            false,
        );

        let loopback_peer = parse_addr("/ip4/127.0.0.2/tcp/5000");
        let addrs = discovery.addresses_for_peer(&loopback_peer);

        // Loopback peer should NOT see NAT address
        assert!(!addrs.contains(&nat_addr));
        assert!(addrs.iter().any(|a| a.to_string().contains("127.0.0.1")));
    }
}
