//! Local node network capabilities tracking.

use std::collections::HashSet;

use libp2p::Multiaddr;
use parking_lot::RwLock;
use tracing::{debug, info};

use crate::scope::{AddressScope, NetworkCapability, classify_multiaddr, extract_ip, same_subnet};
use crate::system::{LocalSubnets, query_local_subnets, refresh_subnets};

/// Local node's network capabilities derived from libp2p listen addresses and OS queries.
///
/// Tracks listen addresses from libp2p events and computes IP/transport capabilities.
/// Integrates with system interface queries for subnet-aware address selection.
pub struct LocalCapabilities {
    listen_addrs: RwLock<Vec<Multiaddr>>,
    capability: RwLock<NetworkCapability>,
}

impl LocalCapabilities {
    pub fn new() -> Self {
        Self {
            listen_addrs: RwLock::new(Vec::new()),
            capability: RwLock::new(NetworkCapability::default()),
        }
    }

    /// Handle new listen address from libp2p.
    ///
    /// Returns `true` if this address caused capability to become known
    /// (transitioned from None to a known capability). Use this to trigger
    /// immediate peer dialing on first address.
    pub fn on_new_listen_addr(&self, addr: Multiaddr) -> bool {
        let mut addrs = self.listen_addrs.write();
        if !addrs.contains(&addr) {
            debug!(listen_addr = %addr, "new listen address");
            let was_unknown = !self.capability.read().is_known();
            addrs.push(addr);
            self.update_capability(&addrs);
            was_unknown && self.capability.read().is_known()
        } else {
            false
        }
    }

    /// Handle expired listen address from libp2p.
    pub fn on_expired_listen_addr(&self, addr: &Multiaddr) {
        let mut addrs = self.listen_addrs.write();
        if let Some(pos) = addrs.iter().position(|a| a == addr) {
            debug!(listen_addr = %addr, "expired listen address");
            addrs.remove(pos);
            self.update_capability(&addrs);
        }
    }

    fn update_capability(&self, listen_addrs: &[Multiaddr]) {
        let new_cap = NetworkCapability::from_addrs(listen_addrs);
        let old_cap = *self.capability.read();
        if new_cap != old_cap {
            info!(?old_cap, ?new_cap, "network capability changed");
            *self.capability.write() = new_cap;
        }
    }

    pub fn capability(&self) -> NetworkCapability {
        *self.capability.read()
    }

    /// Filter addresses to only those we can dial based on our network capability.
    pub fn filter_dialable<'a>(
        &self,
        addrs: &'a [Multiaddr],
    ) -> impl Iterator<Item = &'a Multiaddr> {
        let cap = *self.capability.read();
        addrs.iter().filter(move |addr| cap.can_reach(addr))
    }

    /// Access listen addresses via callback (avoids clone when possible).
    pub fn with_listen_addrs<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&[Multiaddr]) -> R,
    {
        f(&self.listen_addrs.read())
    }

    /// Get a clone of listen addresses (when ownership is needed).
    pub fn listen_addrs(&self) -> Vec<Multiaddr> {
        self.listen_addrs.read().clone()
    }

    /// Number of listen addresses.
    pub fn listen_addr_count(&self) -> usize {
        self.listen_addrs.read().len()
    }

    /// Query local subnets from system interfaces.
    pub fn local_subnets(&self) -> LocalSubnets {
        query_local_subnets()
    }

    /// Force refresh of cached subnet information.
    pub fn refresh_subnets(&self) {
        refresh_subnets();
    }

    /// Check if a target IP is on a directly-connected subnet.
    pub fn is_directly_reachable(&self, addr: &Multiaddr) -> bool {
        extract_ip(addr).map_or(false, crate::system::is_directly_reachable)
    }

    /// Check if two addresses are on the same local subnet.
    pub fn is_on_same_subnet(&self, addr1: &Multiaddr, addr2: &Multiaddr) -> bool {
        same_subnet(addr1, addr2)
    }

    /// Select listen addresses appropriate for a peer's network scope.
    pub fn addresses_for_scope(
        &self,
        peer_scope: AddressScope,
        peer_addr: Option<&Multiaddr>,
    ) -> Vec<Multiaddr> {
        let listen = self.listen_addrs.read();

        // Filter and deduplicate in one pass
        let mut seen = HashSet::new();

        match peer_scope {
            AddressScope::Loopback => listen
                .iter()
                .filter(|addr| {
                    matches!(
                        classify_multiaddr(addr),
                        Some(AddressScope::Loopback | AddressScope::Private)
                    )
                })
                .filter(|addr| seen.insert((*addr).clone()))
                .cloned()
                .collect(),

            AddressScope::Private | AddressScope::LinkLocal => match peer_addr {
                Some(peer) => listen
                    .iter()
                    .filter(|addr| same_subnet(addr, peer))
                    .filter(|addr| seen.insert((*addr).clone()))
                    .cloned()
                    .collect(),
                None => Vec::new(),
            },

            AddressScope::Public => listen
                .iter()
                .filter(|addr| classify_multiaddr(addr) == Some(AddressScope::Public))
                .filter(|addr| seen.insert((*addr).clone()))
                .cloned()
                .collect(),
        }
    }
}

impl Default for LocalCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_addr(s: &str) -> Multiaddr {
        s.parse().unwrap()
    }

    #[test]
    fn test_new_capabilities() {
        let cap = LocalCapabilities::new();
        assert!(cap.listen_addrs().is_empty());
        assert_eq!(cap.listen_addr_count(), 0);
        assert!(!cap.capability().is_known());
    }

    #[test]
    fn test_listen_addr_tracking() {
        let cap = LocalCapabilities::new();

        let addr1 = parse_addr("/ip4/127.0.0.1/tcp/1634");
        let addr2 = parse_addr("/ip6/::1/tcp/1634");

        cap.on_new_listen_addr(addr1.clone());
        cap.on_new_listen_addr(addr2.clone());

        assert_eq!(cap.listen_addr_count(), 2);
        cap.with_listen_addrs(|addrs| {
            assert!(addrs.contains(&addr1));
            assert!(addrs.contains(&addr2));
        });

        cap.on_expired_listen_addr(&addr1);
        assert_eq!(cap.listen_addr_count(), 1);
        cap.with_listen_addrs(|addrs| {
            assert!(!addrs.contains(&addr1));
            assert!(addrs.contains(&addr2));
        });
    }

    #[test]
    fn test_capability_update() {
        let cap = LocalCapabilities::new();

        assert!(!cap.capability().is_known());

        cap.on_new_listen_addr(parse_addr("/ip4/127.0.0.1/tcp/1634"));
        assert!(cap.capability().is_known());

        let net_cap = cap.capability();
        assert!(net_cap.ip.supports_ipv4());
        assert!(!net_cap.ip.supports_ipv6());
        assert!(net_cap.transport.tcp);
        assert!(!net_cap.transport.quic);
    }

    #[test]
    fn test_addresses_for_loopback_peer() {
        let cap = LocalCapabilities::new();

        cap.on_new_listen_addr(parse_addr("/ip4/127.0.0.1/tcp/1634"));
        cap.on_new_listen_addr(parse_addr("/ip4/192.168.1.100/tcp/1634"));
        cap.on_new_listen_addr(parse_addr("/ip4/8.8.8.8/tcp/1634"));

        let addrs = cap.addresses_for_scope(AddressScope::Loopback, None);

        assert!(addrs.iter().any(|a| a.to_string().contains("127.0.0.1")));
        assert!(addrs.iter().any(|a| a.to_string().contains("192.168")));
        assert!(!addrs.iter().any(|a| a.to_string().contains("8.8.8.8")));
    }

    #[test]
    fn test_addresses_for_public_peer() {
        let cap = LocalCapabilities::new();

        cap.on_new_listen_addr(parse_addr("/ip4/192.168.1.100/tcp/1634"));
        cap.on_new_listen_addr(parse_addr("/ip4/8.8.4.4/tcp/1634"));

        let addrs = cap.addresses_for_scope(AddressScope::Public, None);

        assert!(!addrs.iter().any(|a| a.to_string().contains("192.168")));
        assert!(addrs.iter().any(|a| a.to_string().contains("8.8.4.4")));
    }

    #[test]
    fn test_filter_dialable() {
        let cap = LocalCapabilities::new();

        // IPv4 only
        cap.on_new_listen_addr(parse_addr("/ip4/127.0.0.1/tcp/1634"));

        let addrs = vec![
            parse_addr("/ip4/8.8.8.8/tcp/1634"),
            parse_addr("/ip6/::1/tcp/1634"),
        ];

        let dialable: Vec<_> = cap.filter_dialable(&addrs).cloned().collect();
        assert_eq!(dialable.len(), 1);
        assert!(dialable[0].to_string().contains("8.8.8.8"));
    }

    #[test]
    fn test_directly_reachable() {
        let cap = LocalCapabilities::new();

        // Loopback is always reachable
        assert!(cap.is_directly_reachable(&parse_addr("/ip4/127.0.0.1/tcp/1634")));
        assert!(cap.is_directly_reachable(&parse_addr("/ip6/::1/tcp/1634")));

        // Link-local is always reachable
        assert!(cap.is_directly_reachable(&parse_addr("/ip4/169.254.1.1/tcp/1634")));
        assert!(cap.is_directly_reachable(&parse_addr("/ip6/fe80::1/tcp/1634")));
    }

    #[test]
    fn test_with_listen_addrs_callback() {
        let cap = LocalCapabilities::new();
        cap.on_new_listen_addr(parse_addr("/ip4/127.0.0.1/tcp/1634"));

        // Can compute values without cloning
        let count = cap.with_listen_addrs(|addrs| addrs.len());
        assert_eq!(count, 1);

        let has_loopback = cap.with_listen_addrs(|addrs| {
            addrs.iter().any(|a| a.to_string().contains("127.0.0.1"))
        });
        assert!(has_loopback);
    }
}
