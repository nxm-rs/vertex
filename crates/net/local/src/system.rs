//! Local subnet detection via system interface queries.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;

use ipnet::{Ipv4Net, Ipv6Net};
use parking_lot::RwLock;
use tracing::{debug, trace, warn};
use web_time::Instant;

/// How long to cache network interface information before refreshing.
const SUBNET_CACHE_TTL_SECS: u64 = 60;

/// Cached information about local network subnets.
#[derive(Debug, Clone)]
pub struct LocalSubnets {
    ipv4: Vec<Ipv4Net>,
    ipv6: Vec<Ipv6Net>,
    last_refresh: Instant,
}

impl LocalSubnets {
    /// Create by querying system interfaces.
    pub fn from_system() -> Self {
        let interfaces = netdev::get_interfaces();

        let ipv4: Vec<_> = interfaces
            .iter()
            .filter(|iface| iface.is_up())
            .flat_map(|iface| {
                iface.ipv4.iter().filter_map(|net| {
                    let addr = net.addr();
                    if addr.is_loopback() || addr.is_link_local() {
                        None
                    } else {
                        debug!(interface = %iface.name, network = %net, "discovered IPv4 subnet");
                        Some(*net)
                    }
                })
            })
            .collect();

        let ipv6: Vec<_> = interfaces
            .iter()
            .filter(|iface| iface.is_up())
            .flat_map(|iface| {
                iface.ipv6.iter().filter_map(|net| {
                    let addr = net.addr();
                    if addr.is_loopback() || addr.is_unicast_link_local() {
                        None
                    } else {
                        debug!(interface = %iface.name, network = %net, "discovered IPv6 subnet");
                        Some(*net)
                    }
                })
            })
            .collect();

        if ipv4.is_empty() && ipv6.is_empty() {
            warn!("no local subnets discovered - interface query may have failed");
        }

        Self {
            ipv4,
            ipv6,
            last_refresh: Instant::now(),
        }
    }

    /// Iterate over IPv4 subnets.
    pub fn ipv4_subnets(&self) -> impl Iterator<Item = &Ipv4Net> {
        self.ipv4.iter()
    }

    /// Iterate over IPv6 subnets.
    pub fn ipv6_subnets(&self) -> impl Iterator<Item = &Ipv6Net> {
        self.ipv6.iter()
    }

    /// Number of IPv4 subnets.
    pub fn ipv4_count(&self) -> usize {
        self.ipv4.len()
    }

    /// Number of IPv6 subnets.
    pub fn ipv6_count(&self) -> usize {
        self.ipv6.len()
    }

    /// Check if we have no subnets.
    pub fn is_empty(&self) -> bool {
        self.ipv4.is_empty() && self.ipv6.is_empty()
    }

    pub fn contains_ipv4(&self, ip: Ipv4Addr) -> bool {
        self.ipv4.iter().any(|net| net.contains(&ip))
    }

    pub fn contains_ipv6(&self, ip: Ipv6Addr) -> bool {
        self.ipv6.iter().any(|net| net.contains(&ip))
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => self.contains_ipv4(v4),
            IpAddr::V6(v6) => self.contains_ipv6(v6),
        }
    }

    fn is_expired(&self) -> bool {
        self.last_refresh.elapsed().as_secs() >= SUBNET_CACHE_TTL_SECS
    }
}

/// Global cached subnets with auto-refresh.
static SUBNET_CACHE: OnceLock<RwLock<LocalSubnets>> = OnceLock::new();

fn get_cached_subnets() -> LocalSubnets {
    let cache = SUBNET_CACHE.get_or_init(|| RwLock::new(LocalSubnets::from_system()));

    {
        let info = cache.read();
        if !info.is_expired() {
            return info.clone();
        }
    }

    let mut info = cache.write();
    if info.is_expired() {
        debug!("refreshing local subnet cache");
        *info = LocalSubnets::from_system();
    }
    info.clone()
}

/// Check if two IPs are on the same directly-connected subnet.
pub fn is_on_same_subnet(our_ip: IpAddr, target_ip: IpAddr) -> bool {
    match (our_ip, target_ip) {
        (IpAddr::V4(our), IpAddr::V4(target)) => is_on_same_subnet_v4(our, target),
        (IpAddr::V6(our), IpAddr::V6(target)) => is_on_same_subnet_v6(our, target),
        _ => false,
    }
}

fn is_on_same_subnet_v4(our_ip: Ipv4Addr, target_ip: Ipv4Addr) -> bool {
    if our_ip.is_unspecified() || target_ip.is_unspecified() {
        return false;
    }
    if our_ip.is_loopback() && target_ip.is_loopback() {
        return true;
    }
    if our_ip.is_link_local() && target_ip.is_link_local() {
        return true;
    }

    let subnets = get_cached_subnets();
    for subnet in &subnets.ipv4 {
        if subnet.contains(&our_ip) && subnet.contains(&target_ip) {
            trace!(%our_ip, %target_ip, %subnet, "IPs are on same local subnet");
            return true;
        }
    }
    false
}

fn is_on_same_subnet_v6(our_ip: Ipv6Addr, target_ip: Ipv6Addr) -> bool {
    if our_ip.is_unspecified() || target_ip.is_unspecified() {
        return false;
    }
    if our_ip.is_loopback() && target_ip.is_loopback() {
        return true;
    }
    if our_ip.is_unicast_link_local() && target_ip.is_unicast_link_local() {
        return true;
    }

    let subnets = get_cached_subnets();
    for subnet in &subnets.ipv6 {
        if subnet.contains(&our_ip) && subnet.contains(&target_ip) {
            trace!(%our_ip, %target_ip, %subnet, "IPs are on same local subnet");
            return true;
        }
    }
    false
}

/// Check if a target IP is on a directly-connected subnet.
pub fn is_directly_reachable(target_ip: IpAddr) -> bool {
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

/// Force a refresh of the cached subnet information.
pub fn refresh_subnets() {
    if let Some(cache) = SUBNET_CACHE.get() {
        let mut info = cache.write();
        *info = LocalSubnets::from_system();
        debug!("forced refresh of local subnet cache");
    }
}

/// Get a snapshot of local subnets (for diagnostics).
pub fn query_local_subnets() -> LocalSubnets {
    get_cached_subnets()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loopback_same_subnet() {
        let lo1: Ipv4Addr = "127.0.0.1".parse().unwrap();
        let lo2: Ipv4Addr = "127.0.0.2".parse().unwrap();
        assert!(is_on_same_subnet_v4(lo1, lo2));

        let lo6_1: Ipv6Addr = "::1".parse().unwrap();
        let lo6_2: Ipv6Addr = "::1".parse().unwrap();
        assert!(is_on_same_subnet_v6(lo6_1, lo6_2));
    }

    #[test]
    fn test_link_local_same_subnet() {
        let ll1: Ipv4Addr = "169.254.1.1".parse().unwrap();
        let ll2: Ipv4Addr = "169.254.2.2".parse().unwrap();
        assert!(is_on_same_subnet_v4(ll1, ll2));

        let ll6_1: Ipv6Addr = "fe80::1".parse().unwrap();
        let ll6_2: Ipv6Addr = "fe80::2".parse().unwrap();
        assert!(is_on_same_subnet_v6(ll6_1, ll6_2));
    }

    #[test]
    fn test_unspecified_not_on_subnet() {
        let unspec: Ipv4Addr = "0.0.0.0".parse().unwrap();
        let private: Ipv4Addr = "192.168.1.1".parse().unwrap();
        assert!(!is_on_same_subnet_v4(unspec, private));

        let unspec6: Ipv6Addr = "::".parse().unwrap();
        let global: Ipv6Addr = "2001:db8::1".parse().unwrap();
        assert!(!is_on_same_subnet_v6(unspec6, global));
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
    fn test_subnets_from_system() {
        let subnets = LocalSubnets::from_system();
        println!(
            "Discovered {} IPv4 subnets, {} IPv6 subnets",
            subnets.ipv4_count(),
            subnets.ipv6_count()
        );
    }

    #[test]
    fn test_mixed_ip_versions() {
        let v4: IpAddr = "192.168.1.1".parse().unwrap();
        let v6: IpAddr = "::1".parse().unwrap();
        assert!(!is_on_same_subnet(v4, v6));
    }
}
