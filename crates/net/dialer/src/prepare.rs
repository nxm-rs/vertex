//! Dial address preparation with Happy Eyeballs ordering.

use std::num::NonZeroU8;

use libp2p::multiaddr::Protocol;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::{Multiaddr, PeerId};

/// Error returned by [`DialTracker::prepare_and_start`](crate::DialTracker::prepare_and_start).
#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("no reachable addresses after filtering")]
    NoReachableAddresses,
    #[error("peer already pending or in-flight")]
    AlreadyTracked,
    #[error("peer in backoff")]
    InBackoff,
    #[error("peer is banned")]
    Banned,
}

/// IP version extracted from a multiaddr protocol component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IpVersion {
    V4,
    V6,
}

impl IpVersion {
    fn from_multiaddr(addr: &Multiaddr) -> Option<Self> {
        addr.iter().find_map(|p| match p {
            Protocol::Ip4(_) | Protocol::Dns4(_) => Some(Self::V4),
            Protocol::Ip6(_) | Protocol::Dns6(_) => Some(Self::V6),
            _ => None,
        })
    }
}

/// Filter, sort IPv6-first, and compute concurrency factor.
///
/// Returns `None` if no addresses pass the filter.
fn prepare_filtered(
    addrs: impl IntoIterator<Item = Multiaddr>,
    mut filter: impl FnMut(&Multiaddr) -> bool,
) -> Option<(Vec<Multiaddr>, NonZeroU8)> {
    let mut v6 = Vec::new();
    let mut v4 = Vec::new();
    let mut other = Vec::new();

    for addr in addrs {
        if !filter(&addr) {
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

    let sorted: Vec<Multiaddr> = v6.into_iter().chain(v4).chain(other).collect();
    if sorted.is_empty() {
        return None;
    }

    let factor = if ipv6_count > 0 {
        ipv6_count.min(8) as u8
    } else {
        ipv4_count.clamp(1, 4) as u8
    };
    let concurrency = NonZeroU8::new(factor).unwrap_or(NonZeroU8::MIN);

    Some((sorted, concurrency))
}

/// Prepare filtered addresses into `DialOpts` without tracking.
///
/// Returns `None` if no addresses pass the filter. Useful for verification
/// dials that don't need DialTracker bookkeeping.
pub fn prepare_dial_opts(
    peer_id: PeerId,
    addrs: impl IntoIterator<Item = Multiaddr>,
    filter: impl FnMut(&Multiaddr) -> bool,
) -> Option<DialOpts> {
    let (sorted, concurrency) = prepare_filtered(addrs, filter)?;

    Some(
        DialOpts::peer_id(peer_id)
            .addresses(sorted)
            .override_dial_concurrency_factor(concurrency)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer_id() -> PeerId {
        let bytes = [1u8; 32];
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes).unwrap();
        let keypair = libp2p::identity::ed25519::Keypair::from(key);
        PeerId::from_public_key(&libp2p::identity::PublicKey::from(keypair.public()))
    }

    #[test]
    fn test_prepare_filtered_empty() {
        let result = prepare_filtered(Vec::<Multiaddr>::new(), |_| true);
        assert!(result.is_none());
    }

    #[test]
    fn test_prepare_filtered_all_filtered() {
        let addrs: Vec<Multiaddr> = vec!["/ip4/1.2.3.4/tcp/1234".parse().unwrap()];
        let result = prepare_filtered(addrs, |_| false);
        assert!(result.is_none());
    }

    #[test]
    fn test_prepare_filtered_ipv6_first() {
        let addrs: Vec<Multiaddr> = vec![
            "/ip4/1.2.3.4/tcp/1234".parse().unwrap(),
            "/ip6/2001:db8::1/tcp/1234".parse().unwrap(),
            "/ip4/5.6.7.8/tcp/1234".parse().unwrap(),
            "/ip6/2001:db8::2/tcp/1234".parse().unwrap(),
        ];

        let (sorted, concurrency) = prepare_filtered(addrs, |_| true).unwrap();
        assert_eq!(sorted.len(), 4);
        assert!(sorted[0].to_string().contains("ip6"));
        assert!(sorted[1].to_string().contains("ip6"));
        assert!(sorted[2].to_string().contains("ip4"));
        assert!(sorted[3].to_string().contains("ip4"));
        // 2 IPv6 → concurrency = 2
        assert_eq!(concurrency.get(), 2);
    }

    #[test]
    fn test_prepare_filtered_ipv4_only_concurrency() {
        let addrs: Vec<Multiaddr> = vec![
            "/ip4/1.2.3.4/tcp/1234".parse().unwrap(),
            "/ip4/5.6.7.8/tcp/1234".parse().unwrap(),
            "/ip4/9.10.11.12/tcp/1234".parse().unwrap(),
        ];

        let (_, concurrency) = prepare_filtered(addrs, |_| true).unwrap();
        // No IPv6, 3 IPv4 → min(3, 4) = 3
        assert_eq!(concurrency.get(), 3);
    }

    #[test]
    fn test_prepare_filtered_ipv6_capped_at_8() {
        let addrs: Vec<Multiaddr> = (0..12)
            .map(|i| format!("/ip6/2001:db8::{}/tcp/1234", i).parse().unwrap())
            .collect();

        let (_, concurrency) = prepare_filtered(addrs, |_| true).unwrap();
        assert_eq!(concurrency.get(), 8);
    }

    #[test]
    fn test_prepare_dial_opts_returns_none_on_empty() {
        let result = prepare_dial_opts(test_peer_id(), Vec::<Multiaddr>::new(), |_| true);
        assert!(result.is_none());
    }

    #[test]
    fn test_prepare_dial_opts_returns_some() {
        let addrs: Vec<Multiaddr> = vec!["/ip4/1.2.3.4/tcp/1234".parse().unwrap()];
        let result = prepare_dial_opts(test_peer_id(), addrs, |_| true);
        assert!(result.is_some());
    }

    #[test]
    fn test_prepare_filtered_dns_in_other() {
        let addrs: Vec<Multiaddr> = vec![
            "/dnsaddr/example.com/tcp/1234".parse().unwrap(),
            "/ip6/2001:db8::1/tcp/1234".parse().unwrap(),
        ];

        let (sorted, concurrency) = prepare_filtered(addrs, |_| true).unwrap();
        // IPv6 first, then DNS (other)
        assert!(sorted[0].to_string().contains("ip6"));
        assert!(sorted[1].to_string().contains("dnsaddr"));
        // 1 IPv6 → concurrency = 1
        assert_eq!(concurrency.get(), 1);
    }
}
