//! Swarm-specific NAT auto-discovery via peer-observed addresses.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use hashlink::LruCache;
use libp2p::Multiaddr;
use parking_lot::{Mutex, RwLock};
use tracing::{debug, info, trace};
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
pub struct NatDiscovery {
    local: Arc<LocalCapabilities>,
    nat_addrs: Vec<Multiaddr>,
    observed: RwLock<HashMap<Multiaddr, ObservedEntry>>,
    /// Mutex because LruCache::get mutates internal ordering.
    confirmed: Mutex<LruCache<Multiaddr, ConfirmedEntry>>,
    config: NatDiscoveryConfig,
    enabled: bool,
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
        }
    }

    pub fn disabled(local: Arc<LocalCapabilities>) -> Self {
        Self::new(local, vec![], NatDiscoveryConfig::default(), false)
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
    pub fn on_observed_addr(&self, addr: Multiaddr, from_peer: &Multiaddr) {
        trace!(
            observed_addr = %addr,
            peer_addr = %from_peer,
            nat_auto = self.enabled,
            "received observed address from peer"
        );

        if !self.enabled {
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
        let ttl = std::time::Duration::from_secs(self.config.confirmed_ttl_secs);

        // Fast path: check confirmed cache first
        {
            let mut cache = self.confirmed.lock();
            if let Some(entry) = cache.get(&addr) {
                if now.duration_since(entry.confirmed_at) < ttl {
                    trace!(
                        observed_addr = %addr,
                        "confirmed address in cache (fast path)"
                    );
                    return;
                } else {
                    debug!(
                        observed_addr = %addr,
                        "confirmed address expired, removing from cache"
                    );
                    cache.remove(&addr);
                }
            }
        }

        // Normal path: track in observed_addrs until confirmed
        let mut observed = self.observed.write();

        if let Some(entry) = observed.get_mut(&addr) {
            entry.last_seen = now;

            if peer_ip != entry.first_ip {
                entry.confirmations += 1;
                info!(
                    observed_addr = %addr,
                    confirming_ip = %peer_ip,
                    confirmations = entry.confirmations,
                    threshold = self.config.confirmation_threshold,
                    "confirmed by second unique IP"
                );

                if entry.confirmations >= self.config.confirmation_threshold {
                    let addr_to_cache = addr.clone();
                    observed.remove(&addr);
                    drop(observed);
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
            // First observation - check capacity
            if observed.len() >= MAX_OBSERVED_ADDRS {
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

            if self.config.confirmation_threshold <= 1 {
                drop(observed);
                self.add_to_confirmed_cache(addr, now);
            } else {
                debug!(
                    observed_addr = %addr,
                    first_ip = %peer_ip,
                    threshold = self.config.confirmation_threshold,
                    "new observed address pending (1/{} confirmations)",
                    self.config.confirmation_threshold
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
        local_addrs
            .into_iter()
            .chain(nat_addrs)
            .chain(confirmed_addrs)
            .filter(|addr| seen.insert(addr.clone()))
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
