//! Local node network capabilities tracking.

use std::collections::HashSet;

use libp2p::Multiaddr;
use parking_lot::RwLock;
use tracing::{debug, info};

use crate::scope::{AddressScope, IpCapability, classify_multiaddr};
use crate::system::same_subnet;

/// Filter our candidate addresses to those appropriate to advertise to a peer
/// of the given scope.
///
/// This is the single per-scope advertisement rule shared by every place that
/// decides which of our own addresses a peer may learn (the handshake's
/// [`LocalCapabilities::addresses_for_scope`], the identify behaviour, and the
/// dial-eligibility check):
/// - **public** peer: only public-scope candidates,
/// - **private / link-local** peer: only candidates on the same subnet (requires
///   `peer_addr`; without it none qualify),
/// - **loopback** peer: loopback and private candidates.
///
/// Results are deduplicated, preserving input order.
pub fn advertise_filter<'a>(
    candidates: impl Iterator<Item = &'a Multiaddr>,
    peer_scope: AddressScope,
    peer_addr: Option<&Multiaddr>,
) -> Vec<Multiaddr> {
    let mut seen = HashSet::new();
    candidates
        .filter(|addr| match peer_scope {
            AddressScope::Loopback => matches!(
                classify_multiaddr(addr),
                Some(AddressScope::Loopback | AddressScope::Private)
            ),
            AddressScope::Private | AddressScope::LinkLocal => {
                peer_addr.is_some_and(|peer| same_subnet(addr, peer))
            }
            AddressScope::Public => classify_multiaddr(addr) == Some(AddressScope::Public),
        })
        .filter(|addr| seen.insert((*addr).clone()))
        .cloned()
        .collect()
}

/// Local node's network capabilities derived from libp2p listen addresses.
///
/// Tracks listen addresses from libp2p events and computes IP capability.
/// Used for scope-aware address selection during handshake advertisement.
#[derive(Default)]
pub struct LocalCapabilities {
    listen_addrs: RwLock<Vec<Multiaddr>>,
    capability: RwLock<IpCapability>,
    /// Pin the reported capability to [`IpCapability::Dual`] regardless of
    /// listen addresses. Set for dial-only nodes, which never listen and so
    /// can never derive a capability from listeners, yet can dial whatever
    /// address family their host stack routes.
    dial_only: bool,
}

impl LocalCapabilities {
    pub fn new() -> Self {
        Self::default()
    }

    /// Capabilities for a dial-only node: no listeners will ever register,
    /// so the IP capability is pinned to [`IpCapability::Dual`].
    ///
    /// This is the same reasoning the browser target applies globally (see
    /// [`Self::capability`]): an outbound-only node is not limited by what
    /// it listens on, and a dial into an unroutable family fails fast at
    /// the socket rather than poisoning the routing table.
    pub fn dial_only() -> Self {
        Self {
            dial_only: true,
            ..Self::default()
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

    /// The locally observed IP dial capability.
    ///
    /// On native targets this is derived from the node's own listen addresses
    /// (a node with no IPv6 listener does not dial IPv6 peers, and so on),
    /// except for dial-only nodes ([`Self::dial_only`]), which have no
    /// listeners by construction and report [`IpCapability::Dual`]. A
    /// browser client has no listeners either, so on wasm32 the capability is
    /// always [`IpCapability::Dual`]: the browser opens outbound connections
    /// to either address family through its own network stack.
    pub fn capability(&self) -> IpCapability {
        if cfg!(target_arch = "wasm32") || self.dial_only {
            return IpCapability::Dual;
        }
        *self.capability.read()
    }

    /// The combined dial filter for this node: the current IP capability
    /// plus the transport suites this build target's swarm assembly can
    /// dial ([`crate::TransportCapability::platform`]).
    pub fn dial_capability(&self) -> crate::DialCapability {
        crate::DialCapability {
            ip: self.capability(),
            transport: crate::TransportCapability::platform(),
        }
    }

    /// Get a clone of listen addresses.
    pub fn listen_addrs(&self) -> Vec<Multiaddr> {
        self.listen_addrs.read().clone()
    }

    /// Select listen addresses appropriate for a peer's network scope.
    ///
    /// Applies the shared [`advertise_filter`] rule over our listen addresses.
    pub fn addresses_for_scope(
        &self,
        peer_scope: AddressScope,
        peer_addr: Option<&Multiaddr>,
    ) -> Vec<Multiaddr> {
        advertise_filter(self.listen_addrs.read().iter(), peer_scope, peer_addr)
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
    fn dial_only_pins_capability_to_dual() {
        let cap = LocalCapabilities::dial_only();
        assert_eq!(cap.capability(), IpCapability::Dual);
        assert!(cap.capability().is_known());

        // Listen events never arrive for a dial-only node, but even if one
        // did the pin holds: capability stays Dual.
        cap.on_new_listen_addr(parse_addr("/ip4/127.0.0.1/tcp/1634"));
        assert_eq!(cap.capability(), IpCapability::Dual);
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

    // IPv6 scope rules: `::1` is loopback, `fc00::/7` (ULA) is private,
    // `fe80::/10` is link-local, and a global unicast address is public.

    #[test]
    fn advertise_filter_public_peer_keeps_only_global_ipv6() {
        let candidates = [
            parse_addr("/ip6/::1/tcp/1634"),                  // loopback
            parse_addr("/ip6/fd00::1/tcp/1634"),              // ULA (private)
            parse_addr("/ip6/fe80::1/tcp/1634"),              // link-local
            parse_addr("/ip6/2606:4700:4700::1111/tcp/1634"), // global
        ];
        let peer = parse_addr("/ip6/2001:4860:4860::8888/tcp/5000"); // public peer
        let out = advertise_filter(candidates.iter(), AddressScope::Public, Some(&peer));
        assert_eq!(out, vec![parse_addr("/ip6/2606:4700:4700::1111/tcp/1634")]);
    }

    #[test]
    fn advertise_filter_linklocal_ipv6_peer_keeps_same_subnet() {
        // Two IPv6 link-local addresses are always same-subnet; a ULA is not
        // same-subnet as a link-local peer.
        let candidates = [
            parse_addr("/ip6/fe80::abcd/tcp/1634"),
            parse_addr("/ip6/fd00::1/tcp/1634"),
        ];
        let peer = parse_addr("/ip6/fe80::1234/tcp/5000");
        let out = advertise_filter(candidates.iter(), AddressScope::LinkLocal, Some(&peer));
        assert_eq!(out, vec![parse_addr("/ip6/fe80::abcd/tcp/1634")]);
    }

    #[test]
    fn advertise_filter_loopback_peer_keeps_loopback_and_ula_ipv6() {
        let candidates = [
            parse_addr("/ip6/::1/tcp/1634"),                  // loopback
            parse_addr("/ip6/fd00::1/tcp/1634"),              // ULA (private)
            parse_addr("/ip6/2606:4700:4700::1111/tcp/1634"), // global
        ];
        let peer = parse_addr("/ip6/::1/tcp/5000");
        let out = advertise_filter(candidates.iter(), AddressScope::Loopback, Some(&peer));
        assert!(out.contains(&parse_addr("/ip6/::1/tcp/1634")));
        assert!(out.contains(&parse_addr("/ip6/fd00::1/tcp/1634")));
        assert!(!out.contains(&parse_addr("/ip6/2606:4700:4700::1111/tcp/1634")));
    }

    #[test]
    fn advertise_filter_mixed_stack_public_peer() {
        // A dual-stack node advertising to a public peer: both the public IPv4
        // and public IPv6 are kept, private/link-local of either family dropped.
        let candidates = [
            parse_addr("/ip4/192.168.1.10/tcp/1634"),
            parse_addr("/ip4/8.8.4.4/tcp/1634"),
            parse_addr("/ip6/fd00::1/tcp/1634"),
            parse_addr("/ip6/2606:4700:4700::1111/tcp/1634"),
        ];
        let peer = parse_addr("/ip4/8.8.8.8/tcp/5000");
        let out = advertise_filter(candidates.iter(), AddressScope::Public, Some(&peer));
        assert!(out.contains(&parse_addr("/ip4/8.8.4.4/tcp/1634")));
        assert!(out.contains(&parse_addr("/ip6/2606:4700:4700::1111/tcp/1634")));
        assert!(!out.contains(&parse_addr("/ip4/192.168.1.10/tcp/1634")));
        assert!(!out.contains(&parse_addr("/ip6/fd00::1/tcp/1634")));
    }
}
