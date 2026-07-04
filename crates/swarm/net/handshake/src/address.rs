//! Address provider trait for NAT-aware address selection, and the
//! pre-encoding bound on the advertised set.

use std::sync::Arc;

use libp2p::{Multiaddr, PeerId};
use vertex_swarm_peer::MAX_MULTIADDRS_PER_PEER;

use crate::MAX_HANDSHAKE_BUFFER_SIZE;

/// Frame bytes reserved for everything other than the serialized multiaddr
/// block and the welcome message: the signature (65), overlay (32), nonce (32),
/// chequebook (20) and timestamp fields, the network id and node type, the
/// echoed observed multiaddr in a synack, and protobuf tag/length overhead,
/// rounded up for margin.
const FRAME_FIXED_BUDGET: usize = 384;

/// Encoded size of one uvarint.
fn uvarint_len(mut value: u64) -> usize {
    let mut len = 1;
    while value >= 0x80 {
        value >>= 7;
        len += 1;
    }
    len
}

/// Serialized size of one entry in the multiaddr list block: the address bytes
/// plus their uvarint length prefix.
fn entry_len(addr: &Multiaddr) -> usize {
    let len = addr.to_vec().len();
    len + uvarint_len(len as u64)
}

/// Bound the advertised set before it is signed into the self record.
///
/// Keeps the longest prefix of the trust-ordered set that stays within both
/// the record-level count cap and the byte budget left in the handshake frame
/// after the fixed fields and the welcome message. Deterministic prefix
/// truncation: the same input always yields the same output, higher-trust
/// addresses survive, and a non-empty input never bounds to empty (the record
/// must carry at least one multiaddr).
pub(crate) fn bound_advertised(addrs: Vec<Multiaddr>, welcome_bytes: usize) -> Vec<Multiaddr> {
    let budget = MAX_HANDSHAKE_BUFFER_SIZE
        .saturating_sub(FRAME_FIXED_BUDGET)
        .saturating_sub(welcome_bytes);

    // One byte for the list-form block prefix.
    let mut used = 1usize;
    let mut bounded = Vec::new();
    for addr in addrs {
        if bounded.len() == MAX_MULTIADDRS_PER_PEER {
            break;
        }
        used += entry_len(&addr);
        if used > budget && !bounded.is_empty() {
            break;
        }
        bounded.push(addr);
    }
    bounded
}

/// Provides addresses for handshake based on peer context.
///
/// Implementations select appropriate addresses to advertise based on the
/// remote peer's network location (e.g., public vs private, same subnet, etc.).
pub trait AddressProvider: Send + Sync {
    /// Get addresses to advertise to a peer based on their address.
    fn addresses_for_peer(&self, peer_addr: &Multiaddr) -> Vec<Multiaddr>;

    /// Get local peer ID for observed address validation.
    fn local_peer_id(&self) -> Option<&PeerId>;
}

impl<T: AddressProvider> AddressProvider for Arc<T> {
    fn addresses_for_peer(&self, peer_addr: &Multiaddr) -> Vec<Multiaddr> {
        (**self).addresses_for_peer(peer_addr)
    }

    fn local_peer_id(&self) -> Option<&PeerId> {
        (**self).local_peer_id()
    }
}

/// No-op address provider that returns empty addresses.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAddresses;

impl AddressProvider for NoAddresses {
    fn addresses_for_peer(&self, _peer_addr: &Multiaddr) -> Vec<Multiaddr> {
        Vec::new()
    }

    fn local_peer_id(&self) -> Option<&PeerId> {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use vertex_swarm_peer::serialize_multiaddrs;

    use super::*;

    fn ip4_addrs(n: usize) -> Vec<Multiaddr> {
        (0..n)
            .map(|i| {
                format!("/ip4/203.0.113.{}/tcp/1634", i + 1)
                    .parse()
                    .expect("valid multiaddr")
            })
            .collect()
    }

    /// A large entry: dns name plus a /p2p/ suffix, as advertised addresses
    /// carry in practice.
    fn big_addrs(n: usize) -> Vec<Multiaddr> {
        let peer_id = PeerId::random();
        (0..n)
            .map(|i| {
                format!(
                    "/dns4/node-{i:04}.very-long-swarm-hostname.example.org/tcp/1634/p2p/{peer_id}"
                )
                .parse()
                .expect("valid multiaddr")
            })
            .collect()
    }

    #[test]
    fn small_set_passes_unchanged() {
        let addrs = ip4_addrs(3);
        assert_eq!(bound_advertised(addrs.clone(), 0), addrs);
    }

    #[test]
    fn count_cap_binds() {
        let addrs = ip4_addrs(MAX_MULTIADDRS_PER_PEER + 10);
        let bounded = bound_advertised(addrs.clone(), 0);
        assert_eq!(bounded.len(), MAX_MULTIADDRS_PER_PEER);
        assert_eq!(bounded, addrs[..MAX_MULTIADDRS_PER_PEER]);
    }

    #[test]
    fn byte_budget_binds_before_count_cap() {
        let addrs = big_addrs(MAX_MULTIADDRS_PER_PEER);
        let bounded = bound_advertised(addrs.clone(), 0);
        assert!(!bounded.is_empty());
        assert!(bounded.len() < MAX_MULTIADDRS_PER_PEER);
        // The serialized block fits the budget left after the fixed fields.
        let budget = MAX_HANDSHAKE_BUFFER_SIZE - FRAME_FIXED_BUDGET;
        assert!(serialize_multiaddrs(&bounded).len() <= budget);
        // Prefix truncation: the survivors are the head of the input.
        assert_eq!(bounded, addrs[..bounded.len()]);
    }

    #[test]
    fn welcome_message_shrinks_the_budget() {
        let addrs = big_addrs(MAX_MULTIADDRS_PER_PEER);
        let without = bound_advertised(addrs.clone(), 0).len();
        let with = bound_advertised(addrs, 400).len();
        assert!(with < without);
    }

    #[test]
    fn non_empty_input_never_bounds_to_empty() {
        // Even a zero budget keeps the first address: the signed record must
        // carry at least one multiaddr.
        let addrs = big_addrs(3);
        let bounded = bound_advertised(addrs.clone(), MAX_HANDSHAKE_BUFFER_SIZE);
        assert_eq!(bounded, addrs[..1]);
    }

    #[test]
    fn bounding_is_deterministic() {
        let addrs = big_addrs(MAX_MULTIADDRS_PER_PEER);
        assert_eq!(
            bound_advertised(addrs.clone(), 0),
            bound_advertised(addrs, 0)
        );
    }
}
