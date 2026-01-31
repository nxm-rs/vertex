//! IP address scope classification for smart address selection.
//!
//! This module provides utilities to classify IP addresses by scope (loopback,
//! private, link-local, public) and determine subnet membership. These utilities
//! are used by the [`AddressManager`](super::AddressManager) to select appropriate
//! addresses to advertise based on the connecting peer's network scope.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use libp2p::Multiaddr;

/// Classification of IP address scope for smart address selection.
///
/// Used to determine which addresses to advertise based on the scope
/// of the connecting peer.
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

/// Extract the IP address from a multiaddr.
///
/// Returns `None` if the multiaddr doesn't contain an IP protocol.
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

/// Classify the scope of an address in a multiaddr.
///
/// Returns `None` if the multiaddr doesn't contain an IP address or
/// if the IP is unspecified (0.0.0.0, ::).
pub fn classify_multiaddr(addr: &Multiaddr) -> Option<AddressScope> {
    extract_ip(addr).and_then(classify_ip)
}

/// Check if two multiaddrs are on the same local network.
///
/// This queries the system's network interfaces to determine which subnets
/// we're directly connected to, then checks if both addresses fall within
/// the same directly-connected subnet.
///
/// Returns `false` if either address doesn't contain an IP or they use different IP versions.
pub fn same_subnet(addr1: &Multiaddr, addr2: &Multiaddr) -> bool {
    let ip1 = match extract_ip(addr1) {
        Some(ip) => ip,
        None => return false,
    };
    let ip2 = match extract_ip(addr2) {
        Some(ip) => ip,
        None => return false,
    };

    crate::local_network::is_on_same_local_network(ip1, ip2)
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
        let info = crate::local_network::get_local_network_info();

        if let Some(subnet) = info.ipv4_subnets.first() {
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
        let info = crate::local_network::get_local_network_info();
        let google_ip: std::net::Ipv4Addr = "8.8.8.8".parse().unwrap();
        let has_google_dns_subnet = info.ipv4_subnets.iter().any(|s| s.contains(&google_ip));

        if !has_google_dns_subnet {
            assert!(!same_subnet(&addr1, &addr2));
        }
    }
}
