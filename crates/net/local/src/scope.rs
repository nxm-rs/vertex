//! IP address scope classification and capability tracking.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;
use tracing::trace;

/// Classification of IP address scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressScope {
    /// Loopback addresses (127.0.0.0/8, ::1)
    Loopback,
    /// Private addresses (RFC 1918: 10/8, 172.16/12, 192.168/16; RFC 4193: fd00::/8)
    Private,
    /// Link-local addresses (169.254.0.0/16, fe80::/10)
    LinkLocal,
    /// Public/global addresses (everything else)
    Public,
}

/// Extract the IP address from a multiaddr, if any.
pub(crate) fn extract_ip(addr: &Multiaddr) -> Option<IpAddr> {
    for proto in addr.iter() {
        match proto {
            Protocol::Ip4(ip) => return Some(IpAddr::V4(ip)),
            Protocol::Ip6(ip) => return Some(IpAddr::V6(ip)),
            _ => continue,
        }
    }
    None
}

/// Classify the scope of an IP address.
///
/// Returns `None` for unspecified addresses (0.0.0.0, ::) that are not routable.
fn classify_ip(ip: IpAddr) -> Option<AddressScope> {
    match ip {
        IpAddr::V4(ipv4) => classify_ipv4(ipv4),
        IpAddr::V6(ipv6) => classify_ipv6(ipv6),
    }
}

/// Classify an IPv4 address scope.
///
/// Returns `None` for unspecified (0.0.0.0) or broadcast addresses
/// since they're not routable.
fn classify_ipv4(ip: Ipv4Addr) -> Option<AddressScope> {
    if ip.is_unspecified() || ip.is_broadcast() {
        // 0.0.0.0 and 255.255.255.255 are not routable
        None
    } else if ip.is_loopback() {
        Some(AddressScope::Loopback)
    } else if ip.is_private() {
        Some(AddressScope::Private)
    } else if ip.is_link_local() {
        Some(AddressScope::LinkLocal)
    } else {
        Some(AddressScope::Public)
    }
}

/// Classify an IPv6 address scope.
///
/// Returns `None` for unspecified (::) addresses since they're not routable.
fn classify_ipv6(ip: Ipv6Addr) -> Option<AddressScope> {
    if ip.is_unspecified() {
        // :: is not routable
        None
    } else if ip.is_loopback() {
        Some(AddressScope::Loopback)
    } else if ip.is_unique_local() {
        // RFC 4193: fc00::/7 (unique local addresses)
        Some(AddressScope::Private)
    } else if ip.is_unicast_link_local() {
        // fe80::/10
        Some(AddressScope::LinkLocal)
    } else {
        Some(AddressScope::Public)
    }
}

/// Classify the scope of the IP in a multiaddr.
pub fn classify_multiaddr(addr: &Multiaddr) -> Option<AddressScope> {
    extract_ip(addr).and_then(classify_ip)
}

/// IP version of an address (extracted from Protocol).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum IpVersion {
    V4,
    V6,
}

/// Address-family rank for advertisement ordering.
///
/// Lower ranks sort first. IPv6 leads, then IPv4, then anything without an
/// explicit family (DNS, dnsaddr). This is the family tiebreaker used when
/// ordering the addresses a node advertises so peers lead with the families
/// most likely to be globally routable.
///
/// Note: this orders only by family. A node's dial preference may differ; some
/// peers deprioritise IPv6 when dialing. Advertising IPv6-first does not force
/// a peer to dial it first, so this is safe to apply unconditionally on the
/// advertisement path.
///
/// ```
/// use vertex_net_local::AddressFamily;
///
/// let v6: libp2p::Multiaddr = "/ip6/2001:db8::1/tcp/1634".parse().unwrap();
/// let v4: libp2p::Multiaddr = "/ip4/8.8.8.8/tcp/1634".parse().unwrap();
/// let dns: libp2p::Multiaddr = "/dnsaddr/example.com/tcp/1634".parse().unwrap();
/// assert!(AddressFamily::of(&v6) < AddressFamily::of(&v4));
/// assert!(AddressFamily::of(&v4) < AddressFamily::of(&dns));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AddressFamily {
    /// IPv6 (or `/dns6/`).
    V6,
    /// IPv4 (or `/dns4/`).
    V4,
    /// No explicit family (`/dns/`, `/dnsaddr/`, or no IP component).
    Other,
}

impl AddressFamily {
    /// Classify a multiaddr by family for advertisement ordering.
    pub fn of(addr: &Multiaddr) -> Self {
        match IpVersion::from_multiaddr(addr) {
            Some(IpVersion::V6) => Self::V6,
            Some(IpVersion::V4) => Self::V4,
            None => Self::Other,
        }
    }
}

/// Compare two multiaddrs by family for advertisement ordering: IPv6 before
/// IPv4 before family-less addresses.
///
/// This is a partial key, not a total order: addresses of the same family
/// compare `Equal`, so use it with a stable sort to preserve the caller's
/// existing within-family order (for example tier order, or listen-address
/// discovery order).
///
/// ```
/// use vertex_net_local::family_order;
///
/// let mut addrs: Vec<libp2p::Multiaddr> = vec![
///     "/ip4/8.8.8.8/tcp/1634".parse().unwrap(),
///     "/ip6/2001:db8::1/tcp/1634".parse().unwrap(),
/// ];
/// addrs.sort_by(family_order);
/// assert!(addrs[0].to_string().contains("ip6"));
/// ```
pub fn family_order(a: &Multiaddr, b: &Multiaddr) -> std::cmp::Ordering {
    AddressFamily::of(a).cmp(&AddressFamily::of(b))
}

/// Check if a multiaddr is dialable given local IP capability.
///
/// Filters by IP version reachability and non-public subnet reachability.
/// Public addresses and DNS addresses always pass. Non-public addresses
/// (private, link-local) must be on a directly reachable local subnet.
pub fn is_dialable(addr: &Multiaddr, capability: IpCapability) -> bool {
    // Skip addresses we can't reach based on our IP capability
    if !capability.can_reach_addr(addr) {
        return false;
    }

    // Check non-public addresses for local subnet reachability
    if let Some(ip) = extract_ip(addr) {
        match classify_ip(ip) {
            Some(AddressScope::Public) => {}
            Some(scope) => {
                if !crate::system::is_directly_reachable(ip) {
                    trace!(%addr, ?scope, "skipping non-routable address");
                    return false;
                }
            }
            None => {
                trace!(%addr, "skipping unspecified/broadcast address");
                return false;
            }
        }
    }

    true
}

impl IpVersion {
    /// Extract IP version from a libp2p Protocol component.
    ///
    /// Handles both raw IP addresses and DNS variants:
    /// - `Ip4`, `Dns4` → V4
    /// - `Ip6`, `Dns6` → V6
    /// - `Dnsaddr` → None (could resolve to either)
    fn from_protocol(proto: &Protocol) -> Option<Self> {
        match proto {
            Protocol::Ip4(_) | Protocol::Dns4(_) => Some(Self::V4),
            Protocol::Ip6(_) | Protocol::Dns6(_) => Some(Self::V6),
            _ => None,
        }
    }

    /// Extract IP version from a multiaddr.
    pub(crate) fn from_multiaddr(addr: &Multiaddr) -> Option<Self> {
        addr.iter().find_map(|p| Self::from_protocol(&p))
    }
}

/// IP connectivity capability (None, IPv4-only, IPv6-only, or Dual-stack).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum IpCapability {
    #[default]
    None,
    V4Only,
    V6Only,
    Dual,
}

impl IpCapability {
    /// Compute capability from a set of listen addresses.
    pub fn from_addrs<'a>(addrs: impl IntoIterator<Item = &'a Multiaddr>) -> Self {
        let mut has_v4 = false;
        let mut has_v6 = false;

        for addr in addrs {
            match IpVersion::from_multiaddr(addr) {
                Some(IpVersion::V4) => has_v4 = true,
                Some(IpVersion::V6) => has_v6 = true,
                None => {}
            }
            if has_v4 && has_v6 {
                return Self::Dual;
            }
        }

        match (has_v4, has_v6) {
            (true, true) => Self::Dual,
            (true, false) => Self::V4Only,
            (false, true) => Self::V6Only,
            (false, false) => Self::None,
        }
    }

    /// Check if we can reach a multiaddr based on its IP version.
    ///
    /// Returns true for addresses without explicit IP version (e.g., dnsaddr)
    /// since they may resolve to either version.
    pub fn can_reach_addr(&self, addr: &Multiaddr) -> bool {
        match IpVersion::from_multiaddr(addr) {
            Some(IpVersion::V4) => matches!(self, Self::V4Only | Self::Dual),
            Some(IpVersion::V6) => matches!(self, Self::V6Only | Self::Dual),
            None => true,
        }
    }

    /// Check if IP capability is known (we have at least one listen address).
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn supports_ipv4(&self) -> bool {
        matches!(self, Self::V4Only | Self::Dual)
    }

    pub fn supports_ipv6(&self) -> bool {
        matches!(self, Self::V6Only | Self::Dual)
    }

    /// Check if a node with this capability can reach a node with `other` capability.
    ///
    /// Returns true if there's at least one IP version in common.
    /// Returns false if either capability is unknown.
    pub fn can_reach(&self, other: &IpCapability) -> bool {
        if !self.is_known() || !other.is_known() {
            return false;
        }
        (self.supports_ipv4() && other.supports_ipv4())
            || (self.supports_ipv6() && other.supports_ipv6())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use crate::system::same_subnet;

    #[test]
    fn test_classify_ipv4_loopback() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Loopback));

        let addr: Multiaddr = "/ip4/127.255.255.255/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Loopback));
    }

    #[test]
    fn test_classify_ipv4_private() {
        // 10.0.0.0/8
        let addr: Multiaddr = "/ip4/10.0.0.1/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Private));

        let addr: Multiaddr = "/ip4/10.255.255.255/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Private));

        // 172.16.0.0/12
        let addr: Multiaddr = "/ip4/172.16.0.1/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Private));

        let addr: Multiaddr = "/ip4/172.31.255.255/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Private));

        // 192.168.0.0/16
        let addr: Multiaddr = "/ip4/192.168.0.1/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Private));

        let addr: Multiaddr = "/ip4/192.168.255.255/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Private));
    }

    #[test]
    fn test_classify_ipv4_link_local() {
        let addr: Multiaddr = "/ip4/169.254.0.1/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::LinkLocal));

        let addr: Multiaddr = "/ip4/169.254.255.255/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::LinkLocal));
    }

    #[test]
    fn test_classify_ipv4_public() {
        let addr: Multiaddr = "/ip4/8.8.8.8/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Public));

        let addr: Multiaddr = "/ip4/1.1.1.1/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Public));

        // Just outside private range
        let addr: Multiaddr = "/ip4/172.15.255.255/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Public));

        let addr: Multiaddr = "/ip4/172.32.0.0/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Public));
    }

    #[test]
    fn test_classify_ipv6_loopback() {
        let addr: Multiaddr = "/ip6/::1/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Loopback));
    }

    #[test]
    fn test_classify_ipv6_private() {
        // RFC 4193: fd00::/8 (ULA)
        let addr: Multiaddr = "/ip6/fd00::1/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Private));

        let addr: Multiaddr = "/ip6/fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/tcp/1234"
            .parse()
            .unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Private));

        // fc00::/7 includes both fc00::/8 and fd00::/8
        let addr: Multiaddr = "/ip6/fc00::1/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Private));
    }

    #[test]
    fn test_classify_ipv6_link_local() {
        // fe80::/10
        let addr: Multiaddr = "/ip6/fe80::1/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::LinkLocal));

        let addr: Multiaddr = "/ip6/febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff/tcp/1234"
            .parse()
            .unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::LinkLocal));
    }

    #[test]
    fn test_classify_ipv6_public() {
        let addr: Multiaddr = "/ip6/2001:db8::1/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Public));

        let addr: Multiaddr = "/ip6/2607:f8b0:4004:800::200e/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), Some(AddressScope::Public));
    }

    #[test]
    fn test_classify_non_ip_multiaddr() {
        // DNS multiaddr - no IP extracted
        let addr: Multiaddr = "/dns4/example.com/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), None);
    }

    #[test]
    fn test_classify_ipv4_unspecified() {
        // 0.0.0.0 should return None (not routable)
        let addr: Multiaddr = "/ip4/0.0.0.0/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), None);
    }

    #[test]
    fn test_classify_ipv6_unspecified() {
        // :: should return None (not routable)
        let addr: Multiaddr = "/ip6/::/tcp/1234".parse().unwrap();
        assert_eq!(classify_multiaddr(&addr), None);
    }

    #[test]
    fn test_same_subnet_with_unspecified() {
        // Unspecified addresses are not on any subnet
        let unspecified: Multiaddr = "/ip4/0.0.0.0/tcp/1234".parse().unwrap();
        let private: Multiaddr = "/ip4/192.168.1.1/tcp/1234".parse().unwrap();
        assert!(!same_subnet(&unspecified, &private));
        assert!(!same_subnet(&private, &unspecified));
        assert!(!same_subnet(&unspecified, &unspecified));
    }

    #[test]
    fn test_extract_ip() {
        let addr: Multiaddr = "/ip4/192.168.1.1/tcp/1234".parse().unwrap();
        assert_eq!(
            extract_ip(&addr),
            Some(IpAddr::V4("192.168.1.1".parse().unwrap()))
        );

        let addr: Multiaddr = "/ip6/2001:db8::1/tcp/1234".parse().unwrap();
        assert_eq!(
            extract_ip(&addr),
            Some(IpAddr::V6("2001:db8::1".parse().unwrap()))
        );

        let addr: Multiaddr = "/dns4/example.com/tcp/1234".parse().unwrap();
        assert_eq!(extract_ip(&addr), None);
    }

    #[test]
    fn test_same_subnet_ipv4_loopback() {
        // Loopback addresses are always on the same network
        let addr1: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let addr2: Multiaddr = "/ip4/127.0.0.2/tcp/5678".parse().unwrap();
        assert!(same_subnet(&addr1, &addr2));
    }

    #[test]
    fn test_same_subnet_ipv4_link_local() {
        // Link-local addresses (169.254.x.x) are always on the same network
        let addr1: Multiaddr = "/ip4/169.254.1.1/tcp/1234".parse().unwrap();
        let addr2: Multiaddr = "/ip4/169.254.2.2/tcp/5678".parse().unwrap();
        assert!(same_subnet(&addr1, &addr2));
    }

    #[test]
    fn test_same_subnet_ipv6_loopback() {
        // IPv6 loopback addresses are always on the same network
        let addr1: Multiaddr = "/ip6/::1/tcp/1234".parse().unwrap();
        let addr2: Multiaddr = "/ip6/::1/tcp/5678".parse().unwrap();
        assert!(same_subnet(&addr1, &addr2));
    }

    #[test]
    fn test_same_subnet_ipv6_link_local() {
        // IPv6 link-local addresses (fe80::/10) are always on the same network
        let addr1: Multiaddr = "/ip6/fe80::1/tcp/1234".parse().unwrap();
        let addr2: Multiaddr = "/ip6/fe80::2/tcp/5678".parse().unwrap();
        assert!(same_subnet(&addr1, &addr2));
    }

    #[test]
    fn test_same_subnet_mixed_ip_versions() {
        // Different IP versions are never on the same subnet
        let addr1: Multiaddr = "/ip4/192.168.1.1/tcp/1234".parse().unwrap();
        let addr2: Multiaddr = "/ip6/::1/tcp/5678".parse().unwrap();
        assert!(!same_subnet(&addr1, &addr2));
    }

    #[test]
    fn test_same_subnet_uses_actual_interfaces() {
        // This test verifies that same_subnet uses actual system interface info.
        // We get an actual local subnet and test IPs within and outside it.
        let subnets = crate::system::query_local_subnets();

        if let Some(subnet) = subnets.ipv4_subnets().next() {
            // Get host addresses within the subnet
            let hosts: Vec<std::net::Ipv4Addr> = subnet.hosts().take(3).collect();
            if hosts.len() >= 2 {
                let ip1 = hosts[0];
                let ip2 = hosts[1];

                let addr1: Multiaddr = format!("/ip4/{}/tcp/1234", ip1).parse().unwrap();
                let addr2: Multiaddr = format!("/ip4/{}/tcp/5678", ip2).parse().unwrap();

                // Both IPs in the same discovered subnet should be same_subnet
                assert!(
                    same_subnet(&addr1, &addr2),
                    "IPs {} and {} should be on same subnet {}",
                    ip1,
                    ip2,
                    subnet
                );
            }
        } else {
            // No IPv4 subnets discovered - skip this test
            println!("No IPv4 subnets discovered, skipping dynamic subnet test");
        }
    }

    #[test]
    fn test_same_subnet_remote_public_not_local() {
        // Public IPs that are not on any local interface should not be same_subnet
        // (unless by coincidence the test machine has a route to 8.8.8.x)
        let addr1: Multiaddr = "/ip4/8.8.8.8/tcp/1234".parse().unwrap();
        let addr2: Multiaddr = "/ip4/8.8.4.4/tcp/5678".parse().unwrap();

        // These are Google DNS servers - very unlikely to be on a local subnet
        // Unless the test machine happens to have 8.8.8.0/24 as a local subnet,
        // this should return false
        let subnets = crate::system::query_local_subnets();
        let google_ip: std::net::Ipv4Addr = "8.8.8.8".parse().unwrap();
        let has_google_dns_subnet = subnets.ipv4_subnets().any(|s| s.contains(&google_ip));

        if !has_google_dns_subnet {
            assert!(!same_subnet(&addr1, &addr2));
        }
    }

    #[test]
    fn test_is_dialable_public_with_dual() {
        let addr: Multiaddr = "/ip4/8.8.8.8/tcp/1234".parse().unwrap();
        assert!(super::is_dialable(&addr, IpCapability::Dual));
    }

    #[test]
    fn test_is_dialable_ipv6_with_dual() {
        let addr: Multiaddr = "/ip6/2001:db8::1/tcp/1234".parse().unwrap();
        assert!(super::is_dialable(&addr, IpCapability::Dual));
    }

    #[test]
    fn test_is_dialable_filters_by_capability() {
        let v6: Multiaddr = "/ip6/2001:db8::1/tcp/1234".parse().unwrap();
        let v4: Multiaddr = "/ip4/8.8.8.8/tcp/1234".parse().unwrap();

        // IPv4-only should reject IPv6
        assert!(!super::is_dialable(&v6, IpCapability::V4Only));
        assert!(super::is_dialable(&v4, IpCapability::V4Only));

        // IPv6-only should reject IPv4
        assert!(super::is_dialable(&v6, IpCapability::V6Only));
        assert!(!super::is_dialable(&v4, IpCapability::V6Only));
    }

    #[test]
    fn test_is_dialable_none_capability() {
        let v6: Multiaddr = "/ip6/2001:db8::1/tcp/1234".parse().unwrap();
        let v4: Multiaddr = "/ip4/8.8.8.8/tcp/1234".parse().unwrap();

        assert!(!super::is_dialable(&v6, IpCapability::None));
        assert!(!super::is_dialable(&v4, IpCapability::None));
    }

    #[test]
    fn test_is_dialable_dns_always_passes() {
        let addr: Multiaddr = "/dnsaddr/example.com/tcp/1234".parse().unwrap();
        assert!(super::is_dialable(&addr, IpCapability::Dual));
    }

    #[test]
    fn test_is_dialable_loopback() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        assert!(super::is_dialable(&addr, IpCapability::Dual));
    }

    #[test]
    fn test_is_dialable_unspecified() {
        let addr: Multiaddr = "/ip4/0.0.0.0/tcp/1234".parse().unwrap();
        assert!(!super::is_dialable(&addr, IpCapability::Dual));
    }

    #[test]
    fn test_is_dialable_unreachable_private() {
        let addr: Multiaddr = "/ip4/10.233.69.255/tcp/1634".parse().unwrap();
        let subnets = crate::system::query_local_subnets();
        let target: std::net::Ipv4Addr = "10.233.69.255".parse().unwrap();
        if !subnets.contains_ipv4(target) {
            assert!(
                !super::is_dialable(&addr, IpCapability::Dual),
                "private IP not on local subnet should not be dialable"
            );
        }
    }

    #[test]
    fn test_can_reach_symmetric() {
        // can_reach should be symmetric: a.can_reach(b) == b.can_reach(a)
        let all = [
            IpCapability::None,
            IpCapability::V4Only,
            IpCapability::V6Only,
            IpCapability::Dual,
        ];
        for a in &all {
            for b in &all {
                assert_eq!(
                    a.can_reach(b),
                    b.can_reach(a),
                    "can_reach not symmetric for {:?} and {:?}",
                    a,
                    b
                );
            }
        }
    }

    #[test]
    fn test_can_reach_unknown_always_false() {
        let all = [
            IpCapability::None,
            IpCapability::V4Only,
            IpCapability::V6Only,
            IpCapability::Dual,
        ];
        for cap in &all {
            assert!(
                !IpCapability::None.can_reach(cap),
                "None.can_reach({:?}) should be false",
                cap
            );
            assert!(
                !cap.can_reach(&IpCapability::None),
                "{:?}.can_reach(None) should be false",
                cap
            );
        }
    }

    #[test]
    fn test_address_family_of() {
        let v6: Multiaddr = "/ip6/2001:db8::1/tcp/1634".parse().unwrap();
        let v4: Multiaddr = "/ip4/8.8.8.8/tcp/1634".parse().unwrap();
        let dns6: Multiaddr = "/dns6/example.com/tcp/1634".parse().unwrap();
        let dns4: Multiaddr = "/dns4/example.com/tcp/1634".parse().unwrap();
        let dnsaddr: Multiaddr = "/dnsaddr/example.com/tcp/1634".parse().unwrap();

        assert_eq!(AddressFamily::of(&v6), AddressFamily::V6);
        assert_eq!(AddressFamily::of(&v4), AddressFamily::V4);
        assert_eq!(AddressFamily::of(&dns6), AddressFamily::V6);
        assert_eq!(AddressFamily::of(&dns4), AddressFamily::V4);
        assert_eq!(AddressFamily::of(&dnsaddr), AddressFamily::Other);
    }

    #[test]
    fn test_family_order_ipv6_before_ipv4_before_other() {
        assert!(AddressFamily::V6 < AddressFamily::V4);
        assert!(AddressFamily::V4 < AddressFamily::Other);

        let v6: Multiaddr = "/ip6/2001:db8::1/tcp/1634".parse().unwrap();
        let v4: Multiaddr = "/ip4/8.8.8.8/tcp/1634".parse().unwrap();
        let dns: Multiaddr = "/dnsaddr/example.com/tcp/1634".parse().unwrap();

        assert_eq!(family_order(&v6, &v4), std::cmp::Ordering::Less);
        assert_eq!(family_order(&v4, &v6), std::cmp::Ordering::Greater);
        assert_eq!(family_order(&v4, &dns), std::cmp::Ordering::Less);
        assert_eq!(family_order(&v6, &v6), std::cmp::Ordering::Equal);
    }

    #[test]
    fn test_family_order_stable_within_family() {
        // A stable sort with this comparator must preserve input order among
        // addresses of the same family.
        let a: Multiaddr = "/ip4/8.8.8.8/tcp/1634".parse().unwrap();
        let b: Multiaddr = "/ip4/1.1.1.1/tcp/1634".parse().unwrap();
        let c: Multiaddr = "/ip6/2001:db8::1/tcp/1634".parse().unwrap();
        let d: Multiaddr = "/ip6/2001:db8::2/tcp/1634".parse().unwrap();

        let mut addrs = vec![a.clone(), b.clone(), c.clone(), d.clone()];
        addrs.sort_by(family_order);

        // IPv6 first, preserving c before d; then IPv4, preserving a before b.
        assert_eq!(addrs, vec![c, d, a, b]);
    }

    #[test]
    fn test_can_reach_all_known_combinations() {
        // V4 <-> V4: true (shared v4)
        assert!(IpCapability::V4Only.can_reach(&IpCapability::V4Only));
        // V6 <-> V6: true (shared v6)
        assert!(IpCapability::V6Only.can_reach(&IpCapability::V6Only));
        // V4 <-> V6: false (no shared version)
        assert!(!IpCapability::V4Only.can_reach(&IpCapability::V6Only));
        assert!(!IpCapability::V6Only.can_reach(&IpCapability::V4Only));
        // Dual <-> V4: true (shared v4)
        assert!(IpCapability::Dual.can_reach(&IpCapability::V4Only));
        assert!(IpCapability::V4Only.can_reach(&IpCapability::Dual));
        // Dual <-> V6: true (shared v6)
        assert!(IpCapability::Dual.can_reach(&IpCapability::V6Only));
        assert!(IpCapability::V6Only.can_reach(&IpCapability::Dual));
        // Dual <-> Dual: true (shared both)
        assert!(IpCapability::Dual.can_reach(&IpCapability::Dual));
    }
}
