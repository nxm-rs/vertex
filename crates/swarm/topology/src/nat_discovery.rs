//! Local address management for handshake advertisement.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};
use tracing::{debug, info, warn};
use vertex_net_local::{AddressScope, IpCapability, LocalCapabilities, classify_multiaddr};
use vertex_swarm_net_handshake::AddressProvider;

fn strip_peer_id(addr: &Multiaddr) -> Multiaddr {
    addr.iter()
        .filter(|p| !matches!(p, Protocol::P2p(_)))
        .collect()
}

/// Manages local addresses for advertisement during handshake.
///
/// Wraps LocalCapabilities with static NAT addresses, public connectivity
/// tracking, and local PeerId for advertised addresses.
pub struct LocalAddressManager {
    local: Arc<LocalCapabilities>,
    nat_addrs: Vec<Multiaddr>,
    /// Local PeerId for appending /p2p/ to advertised addresses.
    local_peer_id: OnceLock<PeerId>,
    /// Whether we've confirmed public connectivity (peer observed us from public IP).
    has_public_connectivity: AtomicBool,
}

impl LocalAddressManager {
    pub fn new(local: Arc<LocalCapabilities>, nat_addrs: Vec<Multiaddr>) -> Self {
        Self {
            local,
            nat_addrs,
            local_peer_id: OnceLock::new(),
            has_public_connectivity: AtomicBool::new(false),
        }
    }

    /// Create a disabled manager (no NAT addresses).
    pub fn disabled(local: Arc<LocalCapabilities>) -> Self {
        Self::new(local, vec![])
    }

    /// Set the local PeerId for appending /p2p/ to advertised addresses.
    ///
    /// Must be called after the libp2p Swarm is built. Can only be set once.
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

    pub fn local_capabilities(&self) -> &Arc<LocalCapabilities> {
        &self.local
    }

    pub fn nat_addrs(&self) -> &[Multiaddr] {
        &self.nat_addrs
    }

    pub fn capability(&self) -> IpCapability {
        self.local.capability()
    }

    /// Check if we have any public addresses to advertise.
    pub fn has_public_addresses(&self) -> bool {
        // Check static NAT addresses
        if self
            .nat_addrs
            .iter()
            .any(|a| classify_multiaddr(a) == Some(AddressScope::Public))
        {
            return true;
        }

        // Check local listen addresses
        if self
            .local
            .listen_addrs()
            .iter()
            .any(|addr| classify_multiaddr(addr) == Some(AddressScope::Public))
        {
            return true;
        }

        // Check if we've confirmed public connectivity
        self.has_public_connectivity.load(Ordering::Relaxed)
    }

    /// Record an observed address; sets public connectivity flag if address is public.
    pub fn on_observed_addr(&self, addr: &Multiaddr) {
        // Strip /p2p/ suffix if present for classification
        let addr_for_classify = strip_peer_id(addr);

        if classify_multiaddr(&addr_for_classify) == Some(AddressScope::Public)
            && !self.has_public_connectivity.swap(true, Ordering::Relaxed)
        {
            info!("Confirmed public connectivity via peer observation");
        }
    }

    /// Check if public connectivity has been confirmed.
    pub fn has_confirmed_public_connectivity(&self) -> bool {
        self.has_public_connectivity.load(Ordering::Relaxed)
    }

    /// Select addresses to advertise to peer during handshake, filtered by peer scope.
    ///
    /// Does NOT include observed addresses (handshake adds those separately).
    pub fn addresses_for_peer(&self, peer_addr: &Multiaddr) -> Vec<Multiaddr> {
        let peer_scope = classify_multiaddr(peer_addr).unwrap_or(AddressScope::Public);

        // Start with addresses from local capabilities
        let local_addrs = self.local.addresses_for_scope(peer_scope, Some(peer_addr));

        // NAT addresses for non-loopback peers
        let nat_addrs = self
            .nat_addrs
            .iter()
            .filter(|_| peer_scope != AddressScope::Loopback)
            .cloned();

        // Deduplicate
        let mut seen = HashSet::new();
        let addrs: Vec<Multiaddr> = local_addrs
            .into_iter()
            .chain(nat_addrs)
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

    /// All known addresses (listen + NAT).
    pub fn all_addresses(&self) -> Vec<Multiaddr> {
        let listen_addrs = self.local.listen_addrs();
        let nat_addrs = self.nat_addrs.iter().cloned();

        // Deduplicate
        let mut seen = HashSet::new();
        listen_addrs
            .into_iter()
            .chain(nat_addrs)
            .filter(|addr| seen.insert(addr.clone()))
            .collect()
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
}

impl AddressProvider for LocalAddressManager {
    fn addresses_for_peer(&self, peer_addr: &Multiaddr) -> Vec<Multiaddr> {
        self.addresses_for_peer(peer_addr)
    }

    fn local_peer_id(&self) -> Option<&PeerId> {
        self.local_peer_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_addr(s: &str) -> Multiaddr {
        s.parse().unwrap()
    }

    fn create_manager(nat_addrs: Vec<Multiaddr>) -> LocalAddressManager {
        let local = Arc::new(LocalCapabilities::new());
        LocalAddressManager::new(local, nat_addrs)
    }

    #[test]
    fn test_has_public_addresses_with_nat() {
        let nat_addr = parse_addr("/ip4/203.0.113.50/tcp/1634");
        let manager = create_manager(vec![nat_addr]);

        assert!(manager.has_public_addresses());
    }

    #[test]
    fn test_has_public_addresses_with_public_listen() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/8.8.8.8/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        assert!(manager.has_public_addresses());
    }

    #[test]
    fn test_has_public_addresses_none() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/192.168.1.1/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        assert!(!manager.has_public_addresses());
    }

    #[test]
    fn test_addresses_for_peer_includes_nat() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/8.8.4.4/tcp/1634"));

        let nat_addr = parse_addr("/ip4/203.0.113.50/tcp/1634");
        let manager = LocalAddressManager::new(local, vec![nat_addr.clone()]);

        let public_peer = parse_addr("/ip4/8.8.8.8/tcp/5000");
        let addrs = manager.addresses_for_peer(&public_peer);

        // Should include listen address and NAT address
        assert!(addrs.iter().any(|a| a.to_string().contains("8.8.4.4")));
        assert!(addrs.iter().any(|a| a.to_string().contains("203.0.113.50")));
    }

    #[test]
    fn test_nat_addrs_not_for_loopback() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/127.0.0.1/tcp/1634"));

        let nat_addr = parse_addr("/ip4/203.0.113.50/tcp/1634");
        let manager = LocalAddressManager::new(local, vec![nat_addr.clone()]);

        let loopback_peer = parse_addr("/ip4/127.0.0.2/tcp/5000");
        let addrs = manager.addresses_for_peer(&loopback_peer);

        // Loopback peer should NOT see NAT address
        assert!(!addrs.iter().any(|a| a.to_string().contains("203.0.113.50")));
        assert!(addrs.iter().any(|a| a.to_string().contains("127.0.0.1")));
    }

    #[test]
    fn test_all_addresses_deduplicates() {
        let local = Arc::new(LocalCapabilities::new());
        let addr = parse_addr("/ip4/8.8.8.8/tcp/1634");
        local.on_new_listen_addr(addr.clone());

        // Add same addr as NAT (shouldn't duplicate)
        let manager = LocalAddressManager::new(local, vec![addr.clone()]);

        let all = manager.all_addresses();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn test_with_peer_id_appends() {
        let manager = create_manager(vec![]);
        let peer_id = PeerId::random();
        manager.set_local_peer_id(peer_id);

        let addr = parse_addr("/ip4/8.8.8.8/tcp/1634");
        let with_peer_id = manager.with_peer_id(vec![addr]);

        assert_eq!(with_peer_id.len(), 1);
        assert!(with_peer_id[0].to_string().contains(&peer_id.to_string()));
    }

    #[test]
    fn test_observed_public_address_enables_has_public() {
        // Start with no public addresses
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/192.168.1.1/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        // Initially no public connectivity
        assert!(!manager.has_public_addresses());
        assert!(!manager.has_confirmed_public_connectivity());

        // Simulate peer reporting our public address
        let observed = parse_addr("/ip4/91.189.35.149/tcp/1634");
        manager.on_observed_addr(&observed);

        // Now we have confirmed public connectivity
        assert!(manager.has_public_addresses());
        assert!(manager.has_confirmed_public_connectivity());
    }

    #[test]
    fn test_observed_private_address_ignored() {
        let manager = create_manager(vec![]);

        // Private address shouldn't enable public connectivity
        let private_observed = parse_addr("/ip4/192.168.1.100/tcp/1634");
        manager.on_observed_addr(&private_observed);

        assert!(!manager.has_public_addresses());
        assert!(!manager.has_confirmed_public_connectivity());
    }

    #[test]
    fn test_observed_address_with_peer_id_works() {
        let manager = create_manager(vec![]);
        let peer_id = PeerId::random();

        // Observed address often includes /p2p/{peer_id} - should still work
        let observed_with_peer =
            parse_addr(&format!("/ip4/91.189.35.149/tcp/1634/p2p/{}", peer_id));
        manager.on_observed_addr(&observed_with_peer);

        assert!(manager.has_public_addresses());
        assert!(manager.has_confirmed_public_connectivity());
    }

    #[test]
    fn test_multiple_observations_idempotent() {
        let manager = create_manager(vec![]);

        let observed = parse_addr("/ip4/91.189.35.149/tcp/1634");
        manager.on_observed_addr(&observed);
        manager.on_observed_addr(&observed);
        manager.on_observed_addr(&observed);

        // Multiple observations still just mean we have public connectivity
        assert!(manager.has_public_addresses());
        assert!(manager.has_confirmed_public_connectivity());
    }
}
