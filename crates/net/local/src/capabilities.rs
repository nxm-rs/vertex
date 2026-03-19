//! Local node network capabilities tracking.

use std::collections::HashSet;

use libp2p::Multiaddr;
use parking_lot::RwLock;
use tracing::{debug, info};

use crate::scope::{AddressScope, IpCapability, classify_multiaddr};
use crate::system::same_subnet;

/// Local node's network capabilities derived from libp2p listen addresses.
///
/// Tracks listen addresses from libp2p events and computes IP capability.
/// Used for scope-aware address selection during handshake advertisement.
#[derive(Default)]
pub struct LocalCapabilities {
    listen_addrs: RwLock<Vec<Multiaddr>>,
    capability: RwLock<IpCapability>,
}

impl LocalCapabilities {
    pub fn new() -> Self {
        Self {
            listen_addrs: RwLock::new(Vec::new()),
            capability: RwLock::new(IpCapability::default()),
        }
    }

    /// Handle new listen address from libp2p.
    ///
    /// Returns `true` if this address caused capability to become known
    /// (transitioned from unknown to known).
    pub fn on_new_listen_addr(&self, addr: Multiaddr) -> bool {
        let mut addrs = self.listen_addrs.write();
        if addrs.contains(&addr) {
            return false;
        }

        debug!(listen_addr = %addr, "new listen address");
        addrs.push(addr);

        // Hold write lock across check-update-verify to prevent TOCTOU
        let mut cap = self.capability.write();
        let was_unknown = !cap.is_known();
        let new_cap = IpCapability::from_addrs(&*addrs);

        if new_cap != *cap {
            info!(?cap, ?new_cap, "IP capability changed");
            *cap = new_cap;
        }

        was_unknown && cap.is_known()
    }

    /// Handle expired listen address from libp2p.
    pub fn on_expired_listen_addr(&self, addr: &Multiaddr) {
        let mut addrs = self.listen_addrs.write();
        let Some(pos) = addrs.iter().position(|a| a == addr) else {
            return;
        };

        debug!(listen_addr = %addr, "expired listen address");
        addrs.remove(pos);

        let mut cap = self.capability.write();
        let new_cap = IpCapability::from_addrs(&*addrs);

        if new_cap != *cap {
            info!(?cap, ?new_cap, "IP capability changed");
            *cap = new_cap;
        }
    }

    pub fn capability(&self) -> IpCapability {
        *self.capability.read()
    }

    /// Get a clone of listen addresses.
    pub fn listen_addrs(&self) -> Vec<Multiaddr> {
        self.listen_addrs.read().clone()
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
        assert!(!cap.capability().is_known());
    }

    #[test]
    fn test_listen_addr_tracking() {
        let cap = LocalCapabilities::new();

        let addr1 = parse_addr("/ip4/127.0.0.1/tcp/1634");
        let addr2 = parse_addr("/ip6/::1/tcp/1634");

        cap.on_new_listen_addr(addr1.clone());
        cap.on_new_listen_addr(addr2.clone());

        let addrs = cap.listen_addrs();
        assert_eq!(addrs.len(), 2);
        assert!(addrs.contains(&addr1));
        assert!(addrs.contains(&addr2));

        cap.on_expired_listen_addr(&addr1);
        let addrs = cap.listen_addrs();
        assert_eq!(addrs.len(), 1);
        assert!(!addrs.contains(&addr1));
        assert!(addrs.contains(&addr2));
    }

    #[test]
    fn test_capability_update() {
        let cap = LocalCapabilities::new();

        assert!(!cap.capability().is_known());

        cap.on_new_listen_addr(parse_addr("/ip4/127.0.0.1/tcp/1634"));
        assert!(cap.capability().is_known());
        assert!(cap.capability().supports_ipv4());
        assert!(!cap.capability().supports_ipv6());
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
}
