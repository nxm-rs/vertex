//! Local subnet detection via push-based interface watching.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::OnceLock;

use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use libp2p::Multiaddr;
use parking_lot::RwLock;
use tracing::debug;

/// Cached information about local network subnets.
#[derive(Debug, Clone)]
pub(crate) struct LocalSubnets {
    ipv4: Vec<Ipv4Net>,
    ipv6: Vec<Ipv6Net>,
}

impl LocalSubnets {
    fn empty() -> Self {
        Self {
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn ipv4_subnets(&self) -> impl Iterator<Item = &Ipv4Net> {
        self.ipv4.iter()
    }

    pub(crate) fn contains_ipv4(&self, ip: Ipv4Addr) -> bool {
        self.ipv4.iter().any(|net| net.contains(&ip))
    }

    pub(crate) fn contains(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => self.contains_ipv4(v4),
            IpAddr::V6(v6) => self.ipv6.iter().any(|net| net.contains(&v6)),
        }
    }

    /// Check if two IPs belong to the same cached subnet.
    fn contains_pair(&self, a: IpAddr, b: IpAddr) -> bool {
        match (a, b) {
            (IpAddr::V4(a4), IpAddr::V4(b4)) => self
                .ipv4
                .iter()
                .any(|net| net.contains(&a4) && net.contains(&b4)),
            (IpAddr::V6(a6), IpAddr::V6(b6)) => self
                .ipv6
                .iter()
                .any(|net| net.contains(&a6) && net.contains(&b6)),
            _ => false,
        }
    }
}

/// Global cached subnets, populated incrementally by if-watch events.
static SUBNET_CACHE: OnceLock<RwLock<LocalSubnets>> = OnceLock::new();

fn get_cached_subnets() -> LocalSubnets {
    let cache = SUBNET_CACHE.get_or_init(|| RwLock::new(LocalSubnets::empty()));
    cache.read().clone()
}

/// Returns true if the address should be filtered out (loopback or link-local).
fn should_filter(net: &IpNet) -> bool {
    match net {
        IpNet::V4(v4) => {
            let addr = v4.addr();
            addr.is_loopback() || addr.is_link_local()
        }
        IpNet::V6(v6) => {
            let addr = v6.addr();
            addr.is_loopback() || addr.is_unicast_link_local()
        }
    }
}

/// Add a subnet to the cache (called on `IfEvent::Up`).
///
/// Filters loopback and link-local addresses. Returns true if the subnet was added.
pub fn add_subnet(net: IpNet) {
    if should_filter(&net) {
        return;
    }

    let cache = SUBNET_CACHE.get_or_init(|| RwLock::new(LocalSubnets::empty()));
    let mut subnets = cache.write();

    match net {
        IpNet::V4(v4) => {
            if !subnets.ipv4.contains(&v4) {
                debug!(network = %v4, "subnet added");
                subnets.ipv4.push(v4);
            }
        }
        IpNet::V6(v6) => {
            if !subnets.ipv6.contains(&v6) {
                debug!(network = %v6, "subnet added");
                subnets.ipv6.push(v6);
            }
        }
    }
}

/// Remove a subnet from the cache (called on `IfEvent::Down`).
pub fn remove_subnet(net: IpNet) {
    if should_filter(&net) {
        return;
    }

    let Some(cache) = SUBNET_CACHE.get() else {
        return;
    };
    let mut subnets = cache.write();

    match net {
        IpNet::V4(v4) => {
            if let Some(pos) = subnets.ipv4.iter().position(|n| *n == v4) {
                subnets.ipv4.remove(pos);
                debug!(network = %v4, "subnet removed");
            }
        }
        IpNet::V6(v6) => {
            if let Some(pos) = subnets.ipv6.iter().position(|n| *n == v6) {
                subnets.ipv6.remove(pos);
                debug!(network = %v6, "subnet removed");
            }
        }
    }
}

/// Check if two multiaddrs are on the same directly-connected subnet.
pub fn same_subnet(addr1: &Multiaddr, addr2: &Multiaddr) -> bool {
    let (Some(ip1), Some(ip2)) = (crate::scope::extract_ip(addr1), crate::scope::extract_ip(addr2)) else {
        return false;
    };
    is_on_same_subnet(ip1, ip2)
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unicast_link_local(),
    }
}

/// Check if two IPs are on the same directly-connected subnet.
pub(crate) fn is_on_same_subnet(a: IpAddr, b: IpAddr) -> bool {
    if a.is_unspecified() || b.is_unspecified() {
        return false;
    }
    if a.is_loopback() && b.is_loopback() {
        return true;
    }
    if is_link_local(a) && is_link_local(b) {
        return true;
    }

    get_cached_subnets().contains_pair(a, b)
}

/// Check if a target IP is on a directly-connected subnet.
pub(crate) fn is_directly_reachable(target_ip: IpAddr) -> bool {
    if target_ip.is_loopback() {
        return true;
    }
    match target_ip {
        IpAddr::V4(v4) if v4.is_link_local() => return true,
        IpAddr::V6(v6) if v6.is_unicast_link_local() => return true,
        _ => {}
    }
    if target_ip.is_unspecified() {
        return false;
    }

    get_cached_subnets().contains(target_ip)
}

/// Get a snapshot of local subnets (used by sibling module tests).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn query_local_subnets() -> LocalSubnets {
    get_cached_subnets()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loopback_same_subnet() {
        let lo1: IpAddr = "127.0.0.1".parse().unwrap();
        let lo2: IpAddr = "127.0.0.2".parse().unwrap();
        assert!(is_on_same_subnet(lo1, lo2));

        let lo6_1: IpAddr = "::1".parse().unwrap();
        let lo6_2: IpAddr = "::1".parse().unwrap();
        assert!(is_on_same_subnet(lo6_1, lo6_2));
    }

    #[test]
    fn test_link_local_same_subnet() {
        let ll1: IpAddr = "169.254.1.1".parse().unwrap();
        let ll2: IpAddr = "169.254.2.2".parse().unwrap();
        assert!(is_on_same_subnet(ll1, ll2));

        let ll6_1: IpAddr = "fe80::1".parse().unwrap();
        let ll6_2: IpAddr = "fe80::2".parse().unwrap();
        assert!(is_on_same_subnet(ll6_1, ll6_2));
    }

    #[test]
    fn test_unspecified_not_on_subnet() {
        let unspec: IpAddr = "0.0.0.0".parse().unwrap();
        let private: IpAddr = "192.168.1.1".parse().unwrap();
        assert!(!is_on_same_subnet(unspec, private));

        let unspec6: IpAddr = "::".parse().unwrap();
        let global: IpAddr = "2001:db8::1".parse().unwrap();
        assert!(!is_on_same_subnet(unspec6, global));
    }

    #[test]
    fn test_loopback_directly_reachable() {
        assert!(is_directly_reachable("127.0.0.1".parse().unwrap()));
        assert!(is_directly_reachable("::1".parse().unwrap()));
    }

    #[test]
    fn test_link_local_directly_reachable() {
        assert!(is_directly_reachable("169.254.1.1".parse().unwrap()));
        assert!(is_directly_reachable("fe80::1".parse().unwrap()));
    }

    #[test]
    fn test_unspecified_not_reachable() {
        assert!(!is_directly_reachable("0.0.0.0".parse().unwrap()));
        assert!(!is_directly_reachable("::".parse().unwrap()));
    }

    #[test]
    fn test_empty_subnets() {
        let subnets = LocalSubnets::empty();
        assert!(subnets.ipv4.is_empty());
        assert!(subnets.ipv6.is_empty());
    }

    #[test]
    fn test_add_remove_subnet() {
        // Add a subnet
        let net: IpNet = "10.0.0.0/24".parse().unwrap();
        add_subnet(net);

        let subnets = query_local_subnets();
        assert!(subnets.contains("10.0.0.1".parse().unwrap()));

        // Remove it
        remove_subnet(net);
        let subnets = query_local_subnets();
        assert!(!subnets.contains("10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn test_add_subnet_filters_loopback() {
        let net: IpNet = "127.0.0.0/8".parse().unwrap();
        add_subnet(net);

        // Loopback subnets should not be added to the cache
        // (they're handled by special-case logic in is_on_same_subnet)
        let subnets = query_local_subnets();
        // The loopback subnet should have been filtered
        assert!(subnets.ipv4.iter().all(|n| !n.addr().is_loopback()));
    }

    #[test]
    fn test_add_subnet_filters_link_local() {
        let net: IpNet = "169.254.0.0/16".parse().unwrap();
        add_subnet(net);

        let subnets = query_local_subnets();
        assert!(subnets.ipv4.iter().all(|n| !n.addr().is_link_local()));
    }

    #[test]
    fn test_mixed_ip_versions() {
        let v4: IpAddr = "192.168.1.1".parse().unwrap();
        let v6: IpAddr = "::1".parse().unwrap();
        assert!(!is_on_same_subnet(v4, v6));
    }
}
