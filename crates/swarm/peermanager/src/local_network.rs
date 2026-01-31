//! Local network detection using system interface information.
//!
//! This module provides utilities to determine if an IP address is on a directly
//! connected local network by querying the system's network interfaces and their
//! associated subnets.
//!
//! # Cross-Platform Support
//!
//! Uses the `netdev` crate which supports:
//! - Linux, macOS, Windows
//! - Android (tested on 16.0)
//! - iOS (tested on 18.6.2)
//! - BSDs
//!
//! # How It Works
//!
//! Instead of manually comparing IP address bits against assumed subnet sizes,
//! we query the actual network interfaces to get their IP addresses and netmasks.
//! An IP is considered "on the local network" if it falls within any of our
//! directly-connected subnets.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;

use ipnet::{Ipv4Net, Ipv6Net};
use parking_lot::RwLock;
use tracing::{debug, trace, warn};
use web_time::Instant;

/// How long to cache network interface information before refreshing.
/// Network configuration changes are relatively rare, so a longer TTL is fine.
const INTERFACE_CACHE_TTL_SECS: u64 = 60;

/// Cached information about local network interfaces.
#[derive(Debug, Clone)]
pub struct LocalNetworkInfo {
    /// IPv4 subnets we're directly connected to.
    pub ipv4_subnets: Vec<Ipv4Net>,
    /// IPv6 subnets we're directly connected to.
    pub ipv6_subnets: Vec<Ipv6Net>,
    /// When this info was last refreshed.
    pub last_refresh: Instant,
}

impl LocalNetworkInfo {
    /// Create a new LocalNetworkInfo by querying system interfaces.
    pub fn from_system() -> Self {
        let mut ipv4_subnets = Vec::new();
        let mut ipv6_subnets = Vec::new();

        let interfaces = netdev::get_interfaces();
        for iface in interfaces {
            // Skip interfaces that are down
            if !iface.is_up() {
                trace!(interface = %iface.name, "skipping down interface");
                continue;
            }

            // Process IPv4 addresses
            // netdev's ipv4 field contains ipnet::Ipv4Net objects
            for ipv4_net in &iface.ipv4 {
                let addr = ipv4_net.addr();

                // Skip loopback - we handle that separately
                if addr.is_loopback() {
                    continue;
                }
                // Skip link-local (169.254.x.x) - handled separately
                if addr.is_link_local() {
                    continue;
                }

                debug!(
                    interface = %iface.name,
                    network = %ipv4_net,
                    "discovered IPv4 subnet"
                );
                ipv4_subnets.push(*ipv4_net);
            }

            // Process IPv6 addresses
            for ipv6_net in &iface.ipv6 {
                let addr = ipv6_net.addr();

                // Skip loopback
                if addr.is_loopback() {
                    continue;
                }
                // Skip link-local (fe80::/10)
                if addr.is_unicast_link_local() {
                    continue;
                }

                debug!(
                    interface = %iface.name,
                    network = %ipv6_net,
                    "discovered IPv6 subnet"
                );
                ipv6_subnets.push(*ipv6_net);
            }
        }

        if ipv4_subnets.is_empty() && ipv6_subnets.is_empty() {
            warn!("no local network subnets discovered - interface query may have failed");
        }

        Self {
            ipv4_subnets,
            ipv6_subnets,
            last_refresh: Instant::now(),
        }
    }

    /// Check if an IPv4 address is on any of our local subnets.
    pub fn contains_ipv4(&self, ip: Ipv4Addr) -> bool {
        self.ipv4_subnets.iter().any(|net| net.contains(&ip))
    }

    /// Check if an IPv6 address is on any of our local subnets.
    pub fn contains_ipv6(&self, ip: Ipv6Addr) -> bool {
        self.ipv6_subnets.iter().any(|net| net.contains(&ip))
    }

    /// Check if an IP address is on any of our local subnets.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => self.contains_ipv4(v4),
            IpAddr::V6(v6) => self.contains_ipv6(v6),
        }
    }

    /// Check if the cached info has expired.
    pub fn is_expired(&self) -> bool {
        self.last_refresh.elapsed().as_secs() >= INTERFACE_CACHE_TTL_SECS
    }
}

/// Global cached network info with auto-refresh.
static NETWORK_INFO_CACHE: OnceLock<RwLock<LocalNetworkInfo>> = OnceLock::new();

/// Get the cached local network info, refreshing if expired.
fn get_network_info() -> LocalNetworkInfo {
    let cache = NETWORK_INFO_CACHE.get_or_init(|| RwLock::new(LocalNetworkInfo::from_system()));

    // Check if we need to refresh
    {
        let info = cache.read();
        if !info.is_expired() {
            return info.clone();
        }
    }

    // Need to refresh - take write lock
    let mut info = cache.write();
    // Double-check after acquiring write lock
    if info.is_expired() {
        debug!("refreshing local network info cache");
        *info = LocalNetworkInfo::from_system();
    }
    info.clone()
}

/// Check if two IP addresses are on the same local network.
///
/// This queries the system's network interfaces to determine which subnets
/// we're directly connected to, then checks if both addresses fall within
/// the same directly-connected subnet.
///
/// # Arguments
///
/// * `our_ip` - One of our local IP addresses
/// * `target_ip` - The IP address to check
///
/// # Returns
///
/// `true` if both IPs are on the same directly-connected subnet.
///
/// # Special Cases
///
/// - Loopback addresses (127.x.x.x, ::1): Always considered same network if both are loopback
/// - Link-local addresses (169.254.x.x, fe80::/10): Always considered same network if both are link-local
/// - Different IP versions: Always returns false
/// - Unspecified addresses (0.0.0.0, ::): Always returns false
pub fn is_on_same_local_network(our_ip: IpAddr, target_ip: IpAddr) -> bool {
    // Must be same IP version
    match (our_ip, target_ip) {
        (IpAddr::V4(our), IpAddr::V4(target)) => is_on_same_local_network_v4(our, target),
        (IpAddr::V6(our), IpAddr::V6(target)) => is_on_same_local_network_v6(our, target),
        _ => false,
    }
}

fn is_on_same_local_network_v4(our_ip: Ipv4Addr, target_ip: Ipv4Addr) -> bool {
    // Unspecified addresses are not on any network
    if our_ip.is_unspecified() || target_ip.is_unspecified() {
        return false;
    }

    // Loopback is always same network with other loopback
    if our_ip.is_loopback() && target_ip.is_loopback() {
        return true;
    }

    // Link-local is always same network with other link-local
    if our_ip.is_link_local() && target_ip.is_link_local() {
        return true;
    }

    // For regular addresses, check if target is in any of our subnets
    let info = get_network_info();

    // Find which subnet our IP is in, then check if target is in the same subnet
    for subnet in &info.ipv4_subnets {
        if subnet.contains(&our_ip) && subnet.contains(&target_ip) {
            trace!(
                our_ip = %our_ip,
                target_ip = %target_ip,
                subnet = %subnet,
                "IPs are on same local subnet"
            );
            return true;
        }
    }

    false
}

fn is_on_same_local_network_v6(our_ip: Ipv6Addr, target_ip: Ipv6Addr) -> bool {
    // Unspecified addresses are not on any network
    if our_ip.is_unspecified() || target_ip.is_unspecified() {
        return false;
    }

    // Loopback is always same network with other loopback
    if our_ip.is_loopback() && target_ip.is_loopback() {
        return true;
    }

    // Link-local is always same network with other link-local
    if our_ip.is_unicast_link_local() && target_ip.is_unicast_link_local() {
        return true;
    }

    // For regular addresses, check if target is in any of our subnets
    let info = get_network_info();

    for subnet in &info.ipv6_subnets {
        if subnet.contains(&our_ip) && subnet.contains(&target_ip) {
            trace!(
                our_ip = %our_ip,
                target_ip = %target_ip,
                subnet = %subnet,
                "IPs are on same local subnet"
            );
            return true;
        }
    }

    false
}

/// Check if a target IP is directly reachable from our local network.
///
/// This is a simpler check that just verifies if the target IP falls within
/// any of our directly-connected subnets (i.e., we can reach it without
/// going through a gateway).
///
/// # Arguments
///
/// * `target_ip` - The IP address to check
///
/// # Returns
///
/// `true` if the target is on a directly-connected subnet.
pub fn is_directly_reachable(target_ip: IpAddr) -> bool {
    // Loopback is always directly reachable
    if target_ip.is_loopback() {
        return true;
    }

    // Link-local is always directly reachable (by definition)
    match target_ip {
        IpAddr::V4(v4) if v4.is_link_local() => return true,
        IpAddr::V6(v6) if v6.is_unicast_link_local() => return true,
        _ => {}
    }

    // Unspecified is not reachable
    if target_ip.is_unspecified() {
        return false;
    }

    let info = get_network_info();
    info.contains(target_ip)
}

/// Force a refresh of the cached network interface information.
///
/// This is useful after network configuration changes or for testing.
pub fn refresh_network_info() {
    if let Some(cache) = NETWORK_INFO_CACHE.get() {
        let mut info = cache.write();
        *info = LocalNetworkInfo::from_system();
        debug!("forced refresh of local network info cache");
    }
}

/// Get a snapshot of the current local network info (for diagnostics).
pub fn get_local_network_info() -> LocalNetworkInfo {
    get_network_info()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loopback_same_network() {
        let lo1: Ipv4Addr = "127.0.0.1".parse().unwrap();
        let lo2: Ipv4Addr = "127.0.0.2".parse().unwrap();
        assert!(is_on_same_local_network_v4(lo1, lo2));

        let lo6_1: Ipv6Addr = "::1".parse().unwrap();
        let lo6_2: Ipv6Addr = "::1".parse().unwrap();
        assert!(is_on_same_local_network_v6(lo6_1, lo6_2));
    }

    #[test]
    fn test_link_local_same_network() {
        let ll1: Ipv4Addr = "169.254.1.1".parse().unwrap();
        let ll2: Ipv4Addr = "169.254.2.2".parse().unwrap();
        assert!(is_on_same_local_network_v4(ll1, ll2));

        let ll6_1: Ipv6Addr = "fe80::1".parse().unwrap();
        let ll6_2: Ipv6Addr = "fe80::2".parse().unwrap();
        assert!(is_on_same_local_network_v6(ll6_1, ll6_2));
    }

    #[test]
    fn test_unspecified_not_on_network() {
        let unspec: Ipv4Addr = "0.0.0.0".parse().unwrap();
        let private: Ipv4Addr = "192.168.1.1".parse().unwrap();
        assert!(!is_on_same_local_network_v4(unspec, private));
        assert!(!is_on_same_local_network_v4(private, unspec));

        let unspec6: Ipv6Addr = "::".parse().unwrap();
        let global: Ipv6Addr = "2001:db8::1".parse().unwrap();
        assert!(!is_on_same_local_network_v6(unspec6, global));
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
    fn test_network_info_from_system() {
        // This test verifies we can query system interfaces without panicking
        let info = LocalNetworkInfo::from_system();
        // On most systems we should have at least one interface
        // (but don't assert on this as it could fail in containerized environments)
        println!(
            "Discovered {} IPv4 subnets, {} IPv6 subnets",
            info.ipv4_subnets.len(),
            info.ipv6_subnets.len()
        );
        for subnet in &info.ipv4_subnets {
            println!("  IPv4: {}", subnet);
        }
        for subnet in &info.ipv6_subnets {
            println!("  IPv6: {}", subnet);
        }
    }

    #[test]
    fn test_mixed_ip_versions() {
        let v4: IpAddr = "192.168.1.1".parse().unwrap();
        let v6: IpAddr = "::1".parse().unwrap();
        assert!(!is_on_same_local_network(v4, v6));
    }
}
