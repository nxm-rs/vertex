//! Local address management for handshake advertisement.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};
use parking_lot::Mutex;
use tracing::{debug, info, warn};
use vertex_net_local::{
    AddressScope, IpCapability, LocalCapabilities, advertise_filter, classify_multiaddr,
    family_order,
};
use vertex_swarm_net_handshake::AddressProvider;

use crate::reachability::ReachabilityTracker;

fn strip_peer_id(addr: &Multiaddr) -> Multiaddr {
    addr.iter()
        .filter(|p| !matches!(p, Protocol::P2p(_)))
        .collect()
}

/// Manages local addresses for advertisement during handshake.
///
/// Wraps LocalCapabilities with static NAT addresses, reachability
/// tracking, and local PeerId for advertised addresses.
pub struct LocalAddressManager {
    local: Arc<LocalCapabilities>,
    nat_addrs: Vec<Multiaddr>,
    /// Local PeerId for appending /p2p/ to advertised addresses.
    local_peer_id: OnceLock<PeerId>,
    /// Weak, sticky public-connectivity signal: a peer reported observing us at
    /// a public address (see [`Self::on_observed_addr`]). Unverified, so it
    /// never clears.
    observed_reachable: AtomicBool,
    /// Verified public external addresses, confirmed by AutoNAT v2 dial-back or
    /// UPnP port mapping. Unlike the observed signal this is reversible: an
    /// address is removed on `ExternalAddrExpired` (e.g. a UPnP lease lapsing),
    /// so a node whose only public path was a mapping that expired stops
    /// reporting itself reachable.
    confirmed_external_addrs: Mutex<HashSet<Multiaddr>>,
    /// Per-peer reachability bridge. AutoNAT v2 dial-back confirmations
    /// forwarded via [`LocalAddressManager::on_autonat_peer_confirmed`] flow
    /// into this tracker so the kademlia routing layer can score peers by
    /// reachability.
    reachability: ReachabilityTracker,
}

impl LocalAddressManager {
    pub fn new(local: Arc<LocalCapabilities>, nat_addrs: Vec<Multiaddr>) -> Self {
        Self::with_reachability(local, nat_addrs, ReachabilityTracker::new())
    }

    /// Construct a manager sharing an existing [`ReachabilityTracker`]. Useful
    /// when the tracker must also be referenced by routing or external
    /// integrators (e.g. a swarm-builder that wires AutoNAT into the same
    /// tracker without going through the topology behaviour).
    pub fn with_reachability(
        local: Arc<LocalCapabilities>,
        nat_addrs: Vec<Multiaddr>,
        reachability: ReachabilityTracker,
    ) -> Self {
        Self {
            local,
            nat_addrs,
            local_peer_id: OnceLock::new(),
            observed_reachable: AtomicBool::new(false),
            confirmed_external_addrs: Mutex::new(HashSet::new()),
            reachability,
        }
    }

    /// Create a disabled manager (no NAT addresses).
    pub fn disabled(local: Arc<LocalCapabilities>) -> Self {
        Self::new(local, vec![])
    }

    /// Shared per-peer reachability tracker. Cheap to clone (Arc inside).
    pub fn reachability(&self) -> ReachabilityTracker {
        self.reachability.clone()
    }

    /// Record that a peer has been confirmed publicly reachable via an
    /// AutoNAT v2 dial-back.
    ///
    /// The node wiring that owns the `autonat::v2::server::Behaviour` calls
    /// this for each successful dial-back. Promotes the peer to
    /// [`crate::PeerReachability::Reachable`] in the shared tracker.
    pub fn on_autonat_peer_confirmed(&self, peer: PeerId) {
        self.reachability.on_autonat_peer_confirmed(peer);
    }

    /// Register the local PeerId for appending /p2p/ to advertised addresses.
    ///
    /// Must be called after the libp2p Swarm is built. Can only be registered once.
    pub fn register_local_peer_id(&self, peer_id: PeerId) {
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

    /// The combined dial filter (IP capability plus platform transport
    /// suites); see [`LocalCapabilities::dial_capability`].
    pub fn dial_capability(&self) -> vertex_net_local::DialCapability {
        self.local.dial_capability()
    }

    /// Check if we have any public addresses to advertise.
    pub fn is_reachable(&self) -> bool {
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

        // A verified external address (AutoNAT v2 / UPnP) still mapped.
        if !self.confirmed_external_addrs.lock().is_empty() {
            return true;
        }

        // Weak observed-from-public signal.
        self.observed_reachable.load(Ordering::Relaxed)
    }

    /// Record an observed address; sets the reachability flag if the observed address is public-scope.
    pub fn on_observed_addr(&self, addr: &Multiaddr) {
        // Strip /p2p/ suffix if present for classification
        let addr_for_classify = strip_peer_id(addr);

        if classify_multiaddr(&addr_for_classify) == Some(AddressScope::Public)
            && !self.observed_reachable.swap(true, Ordering::Relaxed)
        {
            info!("Confirmed reachability via peer observation");
        }
    }

    /// Record a verified external address.
    ///
    /// Stronger than [`Self::on_observed_addr`]: this is driven by
    /// `FromSwarm::ExternalAddrConfirmed`, which fires only after AutoNAT v2
    /// dial-back or UPnP port mapping proves the address reachable. A confirmed
    /// public address is tracked (reversibly) and enables dials to other public
    /// peers.
    pub fn on_external_addr_confirmed(&self, addr: &Multiaddr) {
        let addr_for_classify = strip_peer_id(addr);

        if classify_multiaddr(&addr_for_classify) == Some(AddressScope::Public)
            && self
                .confirmed_external_addrs
                .lock()
                .insert(addr_for_classify)
        {
            info!(%addr, "Confirmed reachability via verified external address");
        }
    }

    /// Drop a previously-confirmed external address.
    ///
    /// Driven by `FromSwarm::ExternalAddrExpired` (e.g. a UPnP lease that failed
    /// to renew). Once the last verified public address expires, the verified
    /// signal clears; the node may still be public via static NAT addresses,
    /// public listen addresses, or the weak observed signal.
    pub fn on_external_addr_expired(&self, addr: &Multiaddr) {
        let addr_for_classify = strip_peer_id(addr);
        if self
            .confirmed_external_addrs
            .lock()
            .remove(&addr_for_classify)
        {
            debug!(%addr, "Verified external address expired");
        }
    }

    /// Check whether reachability has been confirmed, via either a verified
    /// external address (reversible) or the weak observed signal (sticky).
    pub fn has_confirmed_reachability(&self) -> bool {
        !self.confirmed_external_addrs.lock().is_empty()
            || self.observed_reachable.load(Ordering::Relaxed)
    }

    /// Select addresses to advertise to peer during handshake, filtered by peer
    /// scope and ordered by likely reachability.
    ///
    /// Does NOT include observed addresses. The handshake signs this set as-is
    /// into a cacheable, peer-independent record and only falls back to the
    /// peer-observed address when this set is empty.
    ///
    /// The advertised list is built in reachability tiers and then ordered:
    /// 1. verified-reachable addresses (AutoNAT v2 / UPnP confirmed external),
    /// 2. public listen addresses,
    /// 3. static NAT addresses,
    ///
    /// and within each tier IPv6 leads IPv4. A peer reads this order as a hint
    /// only; its own dial preference may reorder families, so leading with
    /// verified-reachable IPv6 helps without removing any address a peer could
    /// otherwise use.
    pub fn addresses_for_peer(&self, peer_addr: &Multiaddr) -> Vec<Multiaddr> {
        let peer_scope = classify_multiaddr(peer_addr).unwrap_or(AddressScope::Public);

        // Tier 1: verified external addresses (public-scope by construction).
        // Scope-filter consistently with the rest so they never leak to a
        // loopback/private peer they do not apply to.
        let confirmed = self.confirmed_external_addrs.lock();
        let mut verified = advertise_filter(confirmed.iter(), peer_scope, Some(peer_addr));
        drop(confirmed);

        // Tier 2: public (or scope-appropriate) listen addresses.
        let mut listen_addrs = self.local.addresses_for_scope(peer_scope, Some(peer_addr));

        // Tier 3: static NAT addresses for non-loopback peers.
        let mut nat_addrs: Vec<Multiaddr> = self
            .nat_addrs
            .iter()
            .filter(|_| peer_scope != AddressScope::Loopback)
            .cloned()
            .collect();

        // Tier is the primary key; family (IPv6 before IPv4) is the secondary
        // key within a tier. Stable-sort each tier independently, then chain in
        // tier order, so a global family sort never reorders across tiers.
        verified.sort_by(family_order);
        listen_addrs.sort_by(family_order);
        nat_addrs.sort_by(family_order);

        // Deduplicate across tiers, preserving tier order.
        let mut seen = HashSet::new();
        let addrs: Vec<Multiaddr> = verified
            .into_iter()
            .chain(listen_addrs)
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

    /// All known addresses, ordered by likely reachability.
    ///
    /// Tiers mirror [`Self::addresses_for_peer`]: verified external addresses,
    /// then listen addresses, then static NAT addresses, with IPv6 leading IPv4
    /// within each tier. No peer-scope filter is applied here; this is the full
    /// local view.
    pub fn all_addresses(&self) -> Vec<Multiaddr> {
        let mut verified: Vec<Multiaddr> = self
            .confirmed_external_addrs
            .lock()
            .iter()
            .cloned()
            .collect();
        let mut listen_addrs = self.local.listen_addrs();
        let mut nat_addrs = self.nat_addrs.clone();

        verified.sort_by(family_order);
        listen_addrs.sort_by(family_order);
        nat_addrs.sort_by(family_order);

        // Deduplicate across tiers, preserving tier order.
        let mut seen = HashSet::new();
        verified
            .into_iter()
            .chain(listen_addrs)
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
    #![allow(clippy::indexing_slicing)]
    use super::*;

    fn parse_addr(s: &str) -> Multiaddr {
        s.parse().unwrap()
    }

    fn create_manager(nat_addrs: Vec<Multiaddr>) -> LocalAddressManager {
        let local = Arc::new(LocalCapabilities::new());
        LocalAddressManager::new(local, nat_addrs)
    }

    #[test]
    fn test_is_reachable_with_nat() {
        let nat_addr = parse_addr("/ip4/203.0.113.50/tcp/1634");
        let manager = create_manager(vec![nat_addr]);

        assert!(manager.is_reachable());
    }

    #[test]
    fn test_is_reachable_with_public_listen() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/8.8.8.8/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        assert!(manager.is_reachable());
    }

    #[test]
    fn test_is_reachable_none() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/192.168.1.1/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        assert!(!manager.is_reachable());
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

    /// Index of the first address whose string contains `needle`.
    fn position_of(addrs: &[Multiaddr], needle: &str) -> Option<usize> {
        addrs.iter().position(|a| a.to_string().contains(needle))
    }

    #[test]
    fn test_addresses_for_peer_includes_confirmed_external() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/8.8.4.4/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        // A verified external address must be advertised to a public peer.
        manager.on_external_addr_confirmed(&parse_addr("/ip4/203.0.113.7/tcp/1634"));

        let public_peer = parse_addr("/ip4/8.8.8.8/tcp/5000");
        let addrs = manager.addresses_for_peer(&public_peer);

        assert!(addrs.iter().any(|a| a.to_string().contains("203.0.113.7")));
        assert!(addrs.iter().any(|a| a.to_string().contains("8.8.4.4")));
    }

    #[test]
    fn test_addresses_for_peer_verified_first_then_ipv6_within_tier() {
        let local = Arc::new(LocalCapabilities::new());
        // Public listen addresses, both families.
        local.on_new_listen_addr(parse_addr("/ip4/8.8.4.4/tcp/1634"));
        local.on_new_listen_addr(parse_addr("/ip6/2606:4700:4700::1111/tcp/1634"));

        // Static NAT address (tier 3).
        let nat = parse_addr("/ip4/198.51.100.9/tcp/1634");
        let manager = LocalAddressManager::new(local, vec![nat]);

        // Verified external addresses (tier 1), both families.
        manager.on_external_addr_confirmed(&parse_addr("/ip4/203.0.113.7/tcp/1634"));
        manager.on_external_addr_confirmed(&parse_addr("/ip6/2001:db8::7/tcp/1634"));

        let public_peer = parse_addr("/ip4/8.8.8.8/tcp/5000");
        let addrs = manager.addresses_for_peer(&public_peer);

        // Tier ordering: verified before listen before NAT.
        let verified_v6 = position_of(&addrs, "2001:db8::7").unwrap();
        let verified_v4 = position_of(&addrs, "203.0.113.7").unwrap();
        let listen_v6 = position_of(&addrs, "2606:4700:4700::1111").unwrap();
        let listen_v4 = position_of(&addrs, "8.8.4.4").unwrap();
        let nat_v4 = position_of(&addrs, "198.51.100.9").unwrap();

        // Within tier 1: IPv6 before IPv4.
        assert!(verified_v6 < verified_v4);
        // Tier 1 entirely before tier 2.
        assert!(verified_v4 < listen_v6);
        // Within tier 2: IPv6 before IPv4.
        assert!(listen_v6 < listen_v4);
        // Tier 2 entirely before tier 3.
        assert!(listen_v4 < nat_v4);
    }

    #[test]
    fn test_addresses_for_peer_ipv6_public_peer() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip6/2606:4700:4700::1111/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        manager.on_external_addr_confirmed(&parse_addr("/ip6/2001:db8::7/tcp/1634"));

        let public_peer = parse_addr("/ip6/2001:4860:4860::8888/tcp/5000");
        let addrs = manager.addresses_for_peer(&public_peer);

        let verified = position_of(&addrs, "2001:db8::7").unwrap();
        let listen = position_of(&addrs, "2606:4700:4700::1111").unwrap();
        assert!(verified < listen);
    }

    #[test]
    fn test_confirmed_external_not_leaked_to_loopback_peer() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/127.0.0.1/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        manager.on_external_addr_confirmed(&parse_addr("/ip4/203.0.113.7/tcp/1634"));

        let loopback_peer = parse_addr("/ip4/127.0.0.2/tcp/5000");
        let addrs = manager.addresses_for_peer(&loopback_peer);

        // Loopback peer must not learn our public verified address.
        assert!(!addrs.iter().any(|a| a.to_string().contains("203.0.113.7")));
        assert!(addrs.iter().any(|a| a.to_string().contains("127.0.0.1")));
    }

    #[test]
    fn test_confirmed_external_not_leaked_to_private_peer() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/192.168.1.10/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        manager.on_external_addr_confirmed(&parse_addr("/ip4/203.0.113.7/tcp/1634"));

        // Private peer on a different subnet: public scope filter rejects the
        // verified address (advertise_filter same-subnet rule).
        let private_peer = parse_addr("/ip4/10.0.0.5/tcp/5000");
        let addrs = manager.addresses_for_peer(&private_peer);

        assert!(!addrs.iter().any(|a| a.to_string().contains("203.0.113.7")));
    }

    #[test]
    fn test_all_addresses_orders_verified_first_then_ipv6() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/8.8.4.4/tcp/1634"));
        local.on_new_listen_addr(parse_addr("/ip6/2606:4700:4700::1111/tcp/1634"));

        let nat = parse_addr("/ip4/198.51.100.9/tcp/1634");
        let manager = LocalAddressManager::new(local, vec![nat]);

        manager.on_external_addr_confirmed(&parse_addr("/ip6/2001:db8::7/tcp/1634"));

        let all = manager.all_addresses();

        let verified_v6 = position_of(&all, "2001:db8::7").unwrap();
        let listen_v6 = position_of(&all, "2606:4700:4700::1111").unwrap();
        let listen_v4 = position_of(&all, "8.8.4.4").unwrap();
        let nat_v4 = position_of(&all, "198.51.100.9").unwrap();

        // Verified tier first, listen tier IPv6-before-IPv4, NAT tier last.
        assert!(verified_v6 < listen_v6);
        assert!(listen_v6 < listen_v4);
        assert!(listen_v4 < nat_v4);
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
        manager.register_local_peer_id(peer_id);

        let addr = parse_addr("/ip4/8.8.8.8/tcp/1634");
        let with_peer_id = manager.with_peer_id(vec![addr]);

        assert_eq!(with_peer_id.len(), 1);
        assert!(with_peer_id[0].to_string().contains(&peer_id.to_string()));
    }

    #[test]
    fn test_observed_public_address_enables_reachable() {
        // Start with no public addresses
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/192.168.1.1/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        // Initially no reachability
        assert!(!manager.is_reachable());
        assert!(!manager.has_confirmed_reachability());

        // Simulate peer reporting our public address
        let observed = parse_addr("/ip4/91.189.35.149/tcp/1634");
        manager.on_observed_addr(&observed);

        // Now we have confirmed reachability
        assert!(manager.is_reachable());
        assert!(manager.has_confirmed_reachability());
    }

    #[test]
    fn test_external_addr_confirmed_enables_public() {
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/192.168.1.1/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        assert!(!manager.is_reachable());

        // A verified external address (AutoNAT v2 dial-back or UPnP) flips
        // reachability on.
        manager.on_external_addr_confirmed(&parse_addr("/ip4/203.0.113.7/tcp/1634"));

        assert!(manager.has_confirmed_reachability());
        assert!(manager.is_reachable());
    }

    #[test]
    fn test_external_addr_confirmed_private_ignored() {
        let manager = create_manager(vec![]);
        manager.on_external_addr_confirmed(&parse_addr("/ip4/10.0.0.5/tcp/1634"));
        assert!(!manager.has_confirmed_reachability());
    }

    #[test]
    fn test_external_addr_expiry_clears_verified_signal() {
        // A node whose only public path is a (UPnP) mapping must stop
        // reporting reachability once that mapping expires.
        let local = Arc::new(LocalCapabilities::new());
        local.on_new_listen_addr(parse_addr("/ip4/192.168.1.1/tcp/1634"));
        let manager = LocalAddressManager::new(local, vec![]);

        let mapped = parse_addr("/ip4/203.0.113.7/tcp/1634");
        manager.on_external_addr_confirmed(&mapped);
        assert!(manager.is_reachable());

        manager.on_external_addr_expired(&mapped);
        assert!(!manager.has_confirmed_reachability());
        assert!(!manager.is_reachable());
    }

    #[test]
    fn test_observed_signal_survives_external_addr_expiry() {
        // The weak observed signal is sticky and independent of the reversible
        // verified-address set.
        let manager = create_manager(vec![]);
        manager.on_observed_addr(&parse_addr("/ip4/91.189.35.149/tcp/1634"));
        let mapped = parse_addr("/ip4/203.0.113.7/tcp/1634");
        manager.on_external_addr_confirmed(&mapped);
        manager.on_external_addr_expired(&mapped);
        // Observed signal keeps us public.
        assert!(manager.has_confirmed_reachability());
    }

    #[test]
    fn test_observed_private_address_ignored() {
        let manager = create_manager(vec![]);

        // Private address shouldn't enable reachability
        let private_observed = parse_addr("/ip4/192.168.1.100/tcp/1634");
        manager.on_observed_addr(&private_observed);

        assert!(!manager.is_reachable());
        assert!(!manager.has_confirmed_reachability());
    }

    #[test]
    fn test_observed_address_with_peer_id_works() {
        let manager = create_manager(vec![]);
        let peer_id = PeerId::random();

        // Observed address often includes /p2p/{peer_id} - should still work
        let observed_with_peer =
            parse_addr(&format!("/ip4/91.189.35.149/tcp/1634/p2p/{}", peer_id));
        manager.on_observed_addr(&observed_with_peer);

        assert!(manager.is_reachable());
        assert!(manager.has_confirmed_reachability());
    }

    #[test]
    fn test_multiple_observations_idempotent() {
        let manager = create_manager(vec![]);

        let observed = parse_addr("/ip4/91.189.35.149/tcp/1634");
        manager.on_observed_addr(&observed);
        manager.on_observed_addr(&observed);
        manager.on_observed_addr(&observed);

        // Multiple observations still just mean we have reachability
        assert!(manager.is_reachable());
        assert!(manager.has_confirmed_reachability());
    }
}
