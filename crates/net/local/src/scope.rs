//! IP address scope classification and network capability tracking.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;

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
pub fn extract_ip(addr: &Multiaddr) -> Option<IpAddr> {
    use libp2p::multiaddr::Protocol;

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

/// Check if two multiaddrs are on the same directly-connected subnet.
pub fn same_subnet(addr1: &Multiaddr, addr2: &Multiaddr) -> bool {
    let ip1 = match extract_ip(addr1) {
        Some(ip) => ip,
        None => return false,
    };
    let ip2 = match extract_ip(addr2) {
        Some(ip) => ip,
        None => return false,
    };

    crate::system::is_on_same_subnet(ip1, ip2)
}

/// IP version of an address (extracted from Protocol).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum IpVersion {
    V4,
    V6,
}

/// Addresses prepared for dialing with Happy Eyeballs ordering.
#[derive(Debug, Clone)]
pub struct DialAddresses {
    addrs: Vec<Multiaddr>,
    ipv6_count: usize,
    ipv4_count: usize,
}

impl DialAddresses {
    /// Sorted addresses: IPv6 first, then IPv4, then DNS/other.
    pub fn addrs(&self) -> &[Multiaddr] {
        &self.addrs
    }

    /// Consume and return sorted addresses.
    pub fn into_addrs(self) -> Vec<Multiaddr> {
        self.addrs
    }

    /// Number of IPv6 addresses.
    pub fn ipv6_count(&self) -> usize {
        self.ipv6_count
    }

    /// Number of IPv4 addresses.
    pub fn ipv4_count(&self) -> usize {
        self.ipv4_count
    }

    /// Concurrency factor ensuring IPv6 addresses race first.
    ///
    /// Returns IPv6 count (capped at 8) so all IPv6 race in first batch.
    /// IPv4 addresses only start after an IPv6 slot frees up.
    pub fn concurrency_factor(&self) -> std::num::NonZeroU8 {
        use std::num::NonZeroU8;
        let factor = if self.ipv6_count > 0 {
            self.ipv6_count.min(8) as u8
        } else {
            self.ipv4_count.min(4).max(1) as u8
        };
        NonZeroU8::new(factor).unwrap_or(NonZeroU8::MIN)
    }

    /// Check if peer has both IPv6 and IPv4 addresses.
    pub fn is_dual_stack(&self) -> bool {
        self.ipv6_count > 0 && self.ipv4_count > 0
    }

    /// Check if there are any addresses.
    pub fn is_empty(&self) -> bool {
        self.addrs.is_empty()
    }

    /// Total address count.
    pub fn len(&self) -> usize {
        self.addrs.len()
    }
}

/// Prepare multiaddrs for dialing with Happy Eyeballs ordering.
///
/// Filters addresses based on local IP capability, then sorts IPv6 first.
/// This ensures we only attempt addresses we can actually reach, and
/// the concurrency factor accurately reflects dialable addresses.
pub fn prepare_dial_addresses(addrs: Vec<Multiaddr>, capability: IpCapability) -> DialAddresses {
    let mut v6 = Vec::new();
    let mut v4 = Vec::new();
    let mut other = Vec::new();

    for addr in addrs {
        // Skip addresses we can't reach based on our IP capability
        if !capability.can_reach_addr(&addr) {
            continue;
        }

        match IpVersion::from_multiaddr(&addr) {
            Some(IpVersion::V6) => v6.push(addr),
            Some(IpVersion::V4) => v4.push(addr),
            None => other.push(addr),
        }
    }

    let ipv6_count = v6.len();
    let ipv4_count = v4.len();

    DialAddresses {
        addrs: v6.into_iter().chain(v4).chain(other).collect(),
        ipv6_count,
        ipv4_count,
    }
}

impl IpVersion {
    /// Extract IP version from a libp2p Protocol component.
    ///
    /// Handles both raw IP addresses and DNS variants:
    /// - `Ip4`, `Dns4` → V4
    /// - `Ip6`, `Dns6` → V6
    /// - `Dnsaddr` → None (could resolve to either)
    pub fn from_protocol(proto: &Protocol) -> Option<Self> {
        match proto {
            Protocol::Ip4(_) | Protocol::Dns4(_) => Some(Self::V4),
            Protocol::Ip6(_) | Protocol::Dns6(_) => Some(Self::V6),
            _ => None,
        }
    }

    /// Extract IP version from a multiaddr.
    pub fn from_multiaddr(addr: &Multiaddr) -> Option<Self> {
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

    /// Check if we can reach an address with the given IP version.
    pub fn can_reach(&self, version: IpVersion) -> bool {
        match (self, version) {
            (Self::None, _) => false,
            (Self::Dual, _) => true,
            (Self::V4Only, IpVersion::V4) => true,
            (Self::V6Only, IpVersion::V6) => true,
            _ => false,
        }
    }

    /// Check if we can reach a multiaddr.
    ///
    /// Returns true for addresses without explicit IP version (e.g., dnsaddr)
    /// since they may resolve to either version.
    pub fn can_reach_addr(&self, addr: &Multiaddr) -> bool {
        match IpVersion::from_multiaddr(addr) {
            Some(version) => self.can_reach(version),
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

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Create a dual-stack capability.
    pub fn dual_stack() -> Self {
        Self::Dual
    }

    /// Check if this is a dual-stack capability.
    pub fn is_dual_stack(&self) -> bool {
        matches!(self, Self::Dual)
    }
}

/// Get the IP version of a multiaddr, if any.
pub fn ip_version(addr: &Multiaddr) -> Option<IpVersion> {
    IpVersion::from_multiaddr(addr)
}

/// Check if a multiaddr contains an IPv4 address.
pub fn is_ipv4(addr: &Multiaddr) -> bool {
    ip_version(addr) == Some(IpVersion::V4)
}

/// Check if a multiaddr contains an IPv6 address.
pub fn is_ipv6(addr: &Multiaddr) -> bool {
    ip_version(addr) == Some(IpVersion::V6)
}

/// Multiaddr protocol codes for transports we track.
/// From: https://github.com/multiformats/multiaddr/blob/master/protocols.csv
mod proto_code {
    pub(super) const TCP: u32 = 6;
    pub(super) const QUIC_V1: u32 = 461;
    pub(super) const WS: u32 = 477;
    pub(super) const WSS: u32 = 478;
    pub(super) const WEBTRANSPORT: u32 = 465;
}

/// Extract the transport protocol code from a Protocol.
fn transport_code(proto: &Protocol) -> Option<u32> {
    let code = match proto {
        Protocol::Tcp(_) => proto_code::TCP,
        Protocol::QuicV1 => proto_code::QUIC_V1,
        Protocol::Ws(_) => proto_code::WS,
        Protocol::Wss(_) => proto_code::WSS,
        Protocol::WebTransport => proto_code::WEBTRANSPORT,
        _ => return None,
    };
    Some(code)
}

/// Extract the outermost transport protocol code from a multiaddr.
fn transport_code_from_multiaddr(addr: &Multiaddr) -> Option<u32> {
    // Keep the last (outermost) transport found
    // e.g., /tcp/1234/ws -> WS, not TCP
    addr.iter().filter_map(|p| transport_code(&p)).last()
}

/// Set of transport protocols the node can speak.
///
/// Uses multiaddr protocol codes directly for forward compatibility.
/// Codes from: https://github.com/multiformats/multiaddr/blob/master/protocols.csv
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransportCapability {
    codes: HashSet<u32>,
}

impl TransportCapability {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tcp_only() -> Self {
        let mut codes = HashSet::new();
        codes.insert(proto_code::TCP);
        Self { codes }
    }

    pub fn from_addrs<'a>(addrs: impl IntoIterator<Item = &'a Multiaddr>) -> Self {
        let codes = addrs
            .into_iter()
            .filter_map(transport_code_from_multiaddr)
            .collect();
        Self { codes }
    }

    /// Check if we support a specific protocol code.
    pub fn supports_code(&self, code: u32) -> bool {
        self.codes.contains(&code)
    }

    /// Check if we support the transport used by a Protocol.
    pub fn supports(&self, proto: &Protocol) -> bool {
        transport_code(proto)
            .map(|c| self.codes.contains(&c))
            .unwrap_or(true) // Non-transport protocols are always "supported"
    }

    pub fn supports_tcp(&self) -> bool {
        self.codes.contains(&proto_code::TCP)
    }

    pub fn supports_quic(&self) -> bool {
        self.codes.contains(&proto_code::QUIC_V1)
    }

    pub fn supports_websocket(&self) -> bool {
        self.codes.contains(&proto_code::WS) || self.codes.contains(&proto_code::WSS)
    }

    pub fn supports_webtransport(&self) -> bool {
        self.codes.contains(&proto_code::WEBTRANSPORT)
    }

    pub fn can_reach(&self, addr: &Multiaddr) -> bool {
        match transport_code_from_multiaddr(addr) {
            Some(c) => self.codes.contains(&c),
            None => true, // No transport specified - assume reachable
        }
    }

    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    /// Get the protocol codes this capability supports.
    pub fn codes(&self) -> impl Iterator<Item = u32> + '_ {
        self.codes.iter().copied()
    }
}

/// Combined IP + transport capability.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetworkCapability {
    pub ip: IpCapability,
    pub transport: TransportCapability,
}

impl NetworkCapability {
    pub fn from_addrs<'a>(addrs: impl IntoIterator<Item = &'a Multiaddr> + Clone) -> Self {
        Self {
            ip: IpCapability::from_addrs(addrs.clone()),
            transport: TransportCapability::from_addrs(addrs),
        }
    }

    pub fn can_reach(&self, addr: &Multiaddr) -> bool {
        self.ip.can_reach_addr(addr) && self.transport.can_reach(addr)
    }

    pub fn is_known(&self) -> bool {
        self.ip.is_known()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_transport_capability_from_addrs() {
        let tcp: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let quic: Multiaddr = "/ip4/127.0.0.1/udp/1234/quic-v1".parse().unwrap();
        let ws: Multiaddr = "/ip4/127.0.0.1/tcp/1234/ws".parse().unwrap();

        let cap = TransportCapability::from_addrs([&tcp]);
        assert!(cap.supports_tcp());
        assert!(!cap.supports_quic());
        assert!(!cap.supports_websocket());

        let cap = TransportCapability::from_addrs([&quic]);
        assert!(!cap.supports_tcp());
        assert!(cap.supports_quic());
        assert!(!cap.supports_websocket());

        let cap = TransportCapability::from_addrs([&ws]);
        assert!(cap.supports_websocket());

        let cap = TransportCapability::from_addrs([&tcp, &quic]);
        assert!(cap.supports_tcp());
        assert!(cap.supports_quic());
        assert!(!cap.supports_websocket());
    }

    #[test]
    fn test_transport_capability_can_reach() {
        let tcp_only = TransportCapability::tcp_only();
        let tcp_addr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let quic_addr: Multiaddr = "/ip4/127.0.0.1/udp/1234/quic-v1".parse().unwrap();

        assert!(tcp_only.can_reach(&tcp_addr));
        assert!(!tcp_only.can_reach(&quic_addr));
    }

    #[test]
    fn test_network_capability_combined() {
        let ipv4_tcp: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let ipv6_tcp: Multiaddr = "/ip6/::1/tcp/1234".parse().unwrap();

        let cap = NetworkCapability::from_addrs([&ipv4_tcp]);
        assert!(cap.is_known());
        assert!(cap.can_reach(&ipv4_tcp));
        assert!(!cap.can_reach(&ipv6_tcp)); // IPv4 only, can't reach IPv6

        let cap = NetworkCapability::from_addrs([&ipv4_tcp, &ipv6_tcp]);
        assert!(cap.can_reach(&ipv4_tcp));
        assert!(cap.can_reach(&ipv6_tcp));
    }

    #[test]
    fn test_network_capability_transport_filtering() {
        let tcp: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let quic: Multiaddr = "/ip4/127.0.0.1/udp/1234/quic-v1".parse().unwrap();

        // Only TCP listen address
        let cap = NetworkCapability::from_addrs([&tcp]);
        assert!(cap.can_reach(&tcp));
        assert!(!cap.can_reach(&quic)); // Transport mismatch
    }

    #[test]
    fn test_prepare_dial_addresses_ipv6_first() {
        let addrs = vec![
            "/ip4/1.2.3.4/tcp/1234".parse().unwrap(),
            "/ip6/::1/tcp/1234".parse().unwrap(),
            "/ip4/5.6.7.8/tcp/1234".parse().unwrap(),
            "/ip6/2001:db8::1/tcp/1234".parse().unwrap(),
        ];

        let result = super::prepare_dial_addresses(addrs, IpCapability::Dual);

        assert_eq!(result.ipv6_count(), 2);
        assert_eq!(result.ipv4_count(), 2);
        assert!(result.addrs()[0].to_string().contains("ip6"));
        assert!(result.addrs()[1].to_string().contains("ip6"));
        assert!(result.addrs()[2].to_string().contains("ip4"));
        assert!(result.addrs()[3].to_string().contains("ip4"));
    }

    #[test]
    fn test_concurrency_factor_dual_stack() {
        let addrs = vec![
            "/ip6/::1/tcp/1234".parse().unwrap(),
            "/ip6/::2/tcp/1234".parse().unwrap(),
            "/ip6/::3/tcp/1234".parse().unwrap(),
            "/ip4/1.2.3.4/tcp/1234".parse().unwrap(),
            "/ip4/5.6.7.8/tcp/1234".parse().unwrap(),
        ];

        let result = super::prepare_dial_addresses(addrs, IpCapability::Dual);
        // 3 IPv6 addresses → concurrency factor = 3
        assert_eq!(result.concurrency_factor().get(), 3);
        assert!(result.is_dual_stack());
    }

    #[test]
    fn test_concurrency_factor_ipv4_only() {
        let addrs = vec![
            "/ip4/1.2.3.4/tcp/1234".parse().unwrap(),
            "/ip4/5.6.7.8/tcp/1234".parse().unwrap(),
        ];

        let result = super::prepare_dial_addresses(addrs, IpCapability::Dual);
        // No IPv6 → use IPv4 count (capped at 4)
        assert_eq!(result.concurrency_factor().get(), 2);
        assert!(!result.is_dual_stack());
    }

    #[test]
    fn test_concurrency_factor_capped_at_8() {
        let addrs: Vec<_> = (0..12)
            .map(|i| format!("/ip6/2001:db8::{}/tcp/1234", i).parse().unwrap())
            .collect();

        let result = super::prepare_dial_addresses(addrs, IpCapability::Dual);
        // 12 IPv6 addresses → capped at 8
        assert_eq!(result.concurrency_factor().get(), 8);
    }

    #[test]
    fn test_dial_addresses_empty() {
        let result = super::prepare_dial_addresses(Vec::new(), IpCapability::Dual);
        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        // Empty should still return a valid concurrency factor (1)
        assert_eq!(result.concurrency_factor().get(), 1);
    }

    #[test]
    fn test_dial_addresses_dns_sorted_with_ip_version() {
        // dns4 is treated as IPv4, dns6 as IPv6
        let addrs = vec![
            "/dns4/example.com/tcp/1234".parse().unwrap(),
            "/ip6/::1/tcp/1234".parse().unwrap(),
            "/ip4/1.2.3.4/tcp/1234".parse().unwrap(),
        ];

        let result = super::prepare_dial_addresses(addrs, IpCapability::Dual);
        // IPv6 first (ip6), then IPv4 (ip4 and dns4 since dns4 → V4)
        assert_eq!(result.ipv6_count(), 1);
        assert_eq!(result.ipv4_count(), 2); // ip4 + dns4
        assert!(result.addrs()[0].to_string().contains("ip6"));
    }

    #[test]
    fn test_prepare_dial_addresses_filters_by_capability() {
        let addrs = vec![
            "/ip6/::1/tcp/1234".parse().unwrap(),
            "/ip6/::2/tcp/1234".parse().unwrap(),
            "/ip4/1.2.3.4/tcp/1234".parse().unwrap(),
            "/ip4/5.6.7.8/tcp/1234".parse().unwrap(),
        ];

        // IPv4-only capability should filter out IPv6
        let result = super::prepare_dial_addresses(addrs.clone(), IpCapability::V4Only);
        assert_eq!(result.ipv6_count(), 0);
        assert_eq!(result.ipv4_count(), 2);
        assert_eq!(result.len(), 2);

        // IPv6-only capability should filter out IPv4
        let result = super::prepare_dial_addresses(addrs, IpCapability::V6Only);
        assert_eq!(result.ipv6_count(), 2);
        assert_eq!(result.ipv4_count(), 0);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_prepare_dial_addresses_none_capability() {
        let addrs = vec![
            "/ip6/::1/tcp/1234".parse().unwrap(),
            "/ip4/1.2.3.4/tcp/1234".parse().unwrap(),
        ];

        // None capability should filter out all IP addresses
        let result = super::prepare_dial_addresses(addrs, IpCapability::None);
        assert!(result.is_empty());
    }
}
