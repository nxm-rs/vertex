//! Admission gate for gossiped peer records.
//!
//! Records arriving here already passed full signature validation at the
//! hive protocol layer (EIP-191 recovery and overlay recomputation), so the
//! gate only decides whether a record is worth processing: per-overlay
//! cooldown against re-signed re-broadcasts, and a per-gossiper admission
//! budget against floods. Admitted records go straight into the peer
//! manager as unverified, dialable entries; the first completed handshake
//! on a real connection verifies them.

use std::hash::{Hash, Hasher};

use vertex_util_runtime::time::Instant;

use hashlink::LruCache;
use metrics::gauge;

use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::OverlayAddress;

use super::config::GossipConfig;
use super::error::GossipCheckError;
use super::events::GossipCheckOk;
use crate::extract_peer_id;

/// Per-overlay cooldown state: when a record for the overlay was last
/// processed and the fingerprint of the multiaddrs it carried.
struct CooldownState {
    last_processed: Instant,
    addrs_fingerprint: u64,
}

/// Per-gossiper fixed-window admission counter.
struct GossiperBudget {
    window_start: Instant,
    admitted: usize,
}

/// Stateful admission gate for gossiped records.
///
/// All state is LRU-bounded ([`GossipConfig::max_tracked_cooldowns`] and
/// [`GossipConfig::max_tracked_gossipers`]); nothing here dials, queues, or
/// holds peer records.
pub(super) struct GossipIntake {
    /// Per-overlay cooldown entries.
    cooldowns: LruCache<OverlayAddress, CooldownState>,
    /// Per-gossiper admission budgets.
    gossipers: LruCache<OverlayAddress, GossiperBudget>,
    /// Cooldown interval, doubling as the per-gossiper budget window.
    cooldown: std::time::Duration,
    /// Maximum admissions per gossiper per window.
    max_records_per_gossiper: usize,
}

impl GossipIntake {
    pub(super) fn new(config: &GossipConfig) -> Self {
        Self {
            cooldowns: LruCache::new(config.max_tracked_cooldowns),
            gossipers: LruCache::new(config.max_tracked_gossipers),
            cooldown: config.record_cooldown,
            max_records_per_gossiper: config.max_records_per_gossiper,
        }
    }

    /// Decide whether a gossiped record should be admitted to the peer
    /// manager.
    ///
    /// - Records identical to the stored one (same signature, overlapping
    ///   multiaddrs) are skipped as [`GossipCheckOk::AlreadyKnown`].
    /// - Re-signed records whose multiaddrs match the last processed record
    ///   for the overlay are dropped while the cooldown holds; changed
    ///   multiaddrs bypass the cooldown.
    /// - Each gossiper has a bounded admission budget per cooldown window.
    ///
    /// On [`GossipCheckOk::Admitted`] the caller stores the record; the
    /// cooldown clock for the overlay restarts either way.
    pub(super) fn check_gossip(
        &mut self,
        gossiped_peer: &SwarmPeer,
        gossiper: &OverlayAddress,
        existing_peer: Option<&SwarmPeer>,
    ) -> Result<GossipCheckOk, GossipCheckError> {
        let first_addr = gossiped_peer
            .multiaddrs()
            .first()
            .ok_or(GossipCheckError::NoMultiaddrs)?;

        if extract_peer_id(first_addr).is_none() {
            return Err(GossipCheckError::NoPeerId);
        }

        let overlay = OverlayAddress::from(*gossiped_peer.overlay());
        let now = Instant::now();
        let fingerprint = multiaddrs_fingerprint(gossiped_peer);

        if let Some(existing) = existing_peer
            && existing.signature() == gossiped_peer.signature()
            && has_matching_multiaddr(existing, gossiped_peer)
        {
            // Identical record: refresh the cooldown so a re-signed copy
            // arriving next round is suppressed, but admit nothing.
            self.touch_cooldown(overlay, now, fingerprint);
            return Ok(GossipCheckOk::AlreadyKnown);
        }

        if let Some(state) = self.cooldowns.get(&overlay)
            && state.addrs_fingerprint == fingerprint
            && now.duration_since(state.last_processed) < self.cooldown
        {
            return Err(GossipCheckError::CooldownActive);
        }

        self.charge_gossiper(gossiper, now)?;
        self.touch_cooldown(overlay, now, fingerprint);
        Ok(GossipCheckOk::Admitted)
    }

    /// Restart the overlay's cooldown clock with the given fingerprint.
    fn touch_cooldown(&mut self, overlay: OverlayAddress, now: Instant, fingerprint: u64) {
        self.cooldowns.insert(
            overlay,
            CooldownState {
                last_processed: now,
                addrs_fingerprint: fingerprint,
            },
        );
        gauge!("topology_gossip_tracked_cooldowns").set(self.cooldowns.len() as f64);
    }

    /// Charge one admission against the gossiper's window budget.
    fn charge_gossiper(
        &mut self,
        gossiper: &OverlayAddress,
        now: Instant,
    ) -> Result<(), GossipCheckError> {
        if self.gossipers.get(gossiper).is_none() {
            self.gossipers.insert(
                *gossiper,
                GossiperBudget {
                    window_start: now,
                    admitted: 0,
                },
            );
        }
        // Just inserted or already present; either way the entry exists.
        let Some(budget) = self.gossipers.get_mut(gossiper) else {
            return Ok(());
        };
        if now.duration_since(budget.window_start) >= self.cooldown {
            budget.window_start = now;
            budget.admitted = 0;
        }
        if budget.admitted >= self.max_records_per_gossiper {
            return Err(GossipCheckError::GossiperRateLimited);
        }
        budget.admitted += 1;
        gauge!("topology_gossip_tracked_gossipers").set(self.gossipers.len() as f64);
        Ok(())
    }
}

/// Order-insensitive fingerprint of a record's multiaddr set.
fn multiaddrs_fingerprint(peer: &SwarmPeer) -> u64 {
    // XOR of per-address hashes: insensitive to ordering so a reshuffled
    // address list is not treated as news.
    peer.multiaddrs()
        .iter()
        .map(|addr| {
            let mut hasher = std::hash::DefaultHasher::new();
            addr.hash(&mut hasher);
            hasher.finish()
        })
        .fold(0u64, |acc, h| acc ^ h)
}

/// Whether at least one gossiped multiaddr is in the existing record's list.
fn has_matching_multiaddr(existing: &SwarmPeer, gossiped: &SwarmPeer) -> bool {
    gossiped
        .multiaddrs()
        .iter()
        .any(|addr| existing.multiaddrs().contains(addr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, Signature, U256};
    use libp2p::PeerId;
    use std::time::Duration;
    use vertex_swarm_test_utils::test_peer_id;

    fn mock_swarm_peer_with_signature(
        overlay_bytes: [u8; 32],
        addr: &str,
        peer_id: PeerId,
        sig_r: u64,
        sig_s: u64,
    ) -> SwarmPeer {
        let addr_with_p2p = format!("{}/p2p/{}", addr, peer_id);
        let multiaddr: libp2p::Multiaddr = addr_with_p2p.parse().unwrap();
        SwarmPeer::from_parts(
            vec![multiaddr],
            Signature::new(U256::from(sig_r), U256::from(sig_s), false),
            B256::from(overlay_bytes).into(),
            vertex_swarm_primitives::Nonce::ZERO,
            vertex_swarm_peer::Timestamp::from_seconds(1),
            None,
            Address::ZERO,
        )
    }

    fn mock_swarm_peer_with_peer_id(
        overlay_bytes: [u8; 32],
        addr: &str,
        peer_id: PeerId,
    ) -> SwarmPeer {
        mock_swarm_peer_with_signature(overlay_bytes, addr, peer_id, 1, 2)
    }

    fn mock_swarm_peer_no_p2p(overlay_bytes: [u8; 32], addr: &str) -> SwarmPeer {
        let multiaddr: libp2p::Multiaddr = addr.parse().unwrap();
        SwarmPeer::from_parts(
            vec![multiaddr],
            Signature::new(U256::from(1u64), U256::from(2u64), false),
            B256::from(overlay_bytes).into(),
            vertex_swarm_primitives::Nonce::ZERO,
            vertex_swarm_peer::Timestamp::from_seconds(1),
            None,
            Address::ZERO,
        )
    }

    fn gossiper(byte: u8) -> OverlayAddress {
        OverlayAddress::from(B256::from([byte; 32]))
    }

    #[test]
    fn test_rejects_no_peer_id() {
        let mut intake = GossipIntake::new(&GossipConfig::default());

        let peer = mock_swarm_peer_no_p2p([2u8; 32], "/ip4/1.2.3.4/tcp/1634");
        let result = intake.check_gossip(&peer, &gossiper(3), None);
        assert!(matches!(result, Err(GossipCheckError::NoPeerId)));
    }

    #[test]
    fn test_admits_new_record() {
        let mut intake = GossipIntake::new(&GossipConfig::default());

        let peer =
            mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", test_peer_id(2));
        let result = intake.check_gossip(&peer, &gossiper(3), None);
        assert!(matches!(result, Ok(GossipCheckOk::Admitted)));
    }

    #[test]
    fn test_already_known_with_matching_signature() {
        let mut intake = GossipIntake::new(&GossipConfig::default());

        let peer =
            mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", test_peer_id(2));
        let existing = peer.clone();
        let result = intake.check_gossip(&peer, &gossiper(3), Some(&existing));
        assert!(matches!(result, Ok(GossipCheckOk::AlreadyKnown)));
    }

    #[test]
    fn test_cooldown_suppresses_resigned_record_with_same_addrs() {
        let mut intake = GossipIntake::new(&GossipConfig::default());
        let peer_id = test_peer_id(2);

        let first =
            mock_swarm_peer_with_signature([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id, 1, 2);
        assert!(matches!(
            intake.check_gossip(&first, &gossiper(3), None),
            Ok(GossipCheckOk::Admitted)
        ));

        // Same overlay, same multiaddrs, fresh signature: no news.
        let resigned =
            mock_swarm_peer_with_signature([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id, 3, 4);
        assert!(matches!(
            intake.check_gossip(&resigned, &gossiper(3), Some(&first)),
            Err(GossipCheckError::CooldownActive)
        ));

        // A different gossiper relaying the same re-signed record is
        // suppressed too: the cooldown is per overlay, not per source.
        assert!(matches!(
            intake.check_gossip(&resigned, &gossiper(9), Some(&first)),
            Err(GossipCheckError::CooldownActive)
        ));
    }

    #[test]
    fn test_changed_multiaddrs_bypass_cooldown() {
        let mut intake = GossipIntake::new(&GossipConfig::default());
        let peer_id = test_peer_id(2);

        let first =
            mock_swarm_peer_with_signature([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id, 1, 2);
        assert!(matches!(
            intake.check_gossip(&first, &gossiper(3), None),
            Ok(GossipCheckOk::Admitted)
        ));

        // Same overlay, new multiaddr: real news, admitted immediately.
        let moved =
            mock_swarm_peer_with_signature([2u8; 32], "/ip4/5.6.7.8/tcp/1634", peer_id, 3, 4);
        assert!(matches!(
            intake.check_gossip(&moved, &gossiper(3), Some(&first)),
            Ok(GossipCheckOk::Admitted)
        ));
    }

    #[test]
    fn test_cooldown_expiry_readmits() {
        let config = GossipConfig {
            record_cooldown: Duration::ZERO,
            ..Default::default()
        };
        let mut intake = GossipIntake::new(&config);
        let peer_id = test_peer_id(2);

        let first =
            mock_swarm_peer_with_signature([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id, 1, 2);
        assert!(matches!(
            intake.check_gossip(&first, &gossiper(3), None),
            Ok(GossipCheckOk::Admitted)
        ));

        let resigned =
            mock_swarm_peer_with_signature([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id, 3, 4);
        assert!(matches!(
            intake.check_gossip(&resigned, &gossiper(3), Some(&first)),
            Ok(GossipCheckOk::Admitted),
        ));
    }

    #[test]
    fn test_already_known_starts_cooldown() {
        let mut intake = GossipIntake::new(&GossipConfig::default());
        let peer_id = test_peer_id(2);

        let stored =
            mock_swarm_peer_with_signature([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id, 1, 2);
        assert!(matches!(
            intake.check_gossip(&stored, &gossiper(3), Some(&stored)),
            Ok(GossipCheckOk::AlreadyKnown)
        ));

        // A re-signed copy arriving right after the identical one is
        // suppressed by the cooldown the AlreadyKnown pass started.
        let resigned =
            mock_swarm_peer_with_signature([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id, 3, 4);
        assert!(matches!(
            intake.check_gossip(&resigned, &gossiper(3), Some(&stored)),
            Err(GossipCheckError::CooldownActive)
        ));
    }

    #[test]
    fn test_rate_limit_per_gossiper() {
        // Tightened config: a small per-gossiper budget keeps the test fast
        // and proves the limit is config-driven rather than hardcoded.
        let max_records_per_gossiper = 4;
        let config = GossipConfig {
            max_records_per_gossiper,
            ..Default::default()
        };
        let mut intake = GossipIntake::new(&config);
        let source = gossiper(3);

        for i in 0..max_records_per_gossiper {
            let peer_id = test_peer_id((i + 10) as u8);
            let mut overlay_bytes = [0u8; 32];
            overlay_bytes[0] = (i + 10) as u8;
            let peer =
                mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);
            assert!(matches!(
                intake.check_gossip(&peer, &source, None),
                Ok(GossipCheckOk::Admitted)
            ));
        }

        // Budget exhausted: the next admission from this source is rejected.
        let peer =
            mock_swarm_peer_with_peer_id([100u8; 32], "/ip4/1.2.3.4/tcp/1634", test_peer_id(100));
        assert!(matches!(
            intake.check_gossip(&peer, &source, None),
            Err(GossipCheckError::GossiperRateLimited)
        ));

        // Another gossiper still has its own budget.
        let peer =
            mock_swarm_peer_with_peer_id([101u8; 32], "/ip4/1.2.3.4/tcp/1634", test_peer_id(101));
        assert!(matches!(
            intake.check_gossip(&peer, &gossiper(7), None),
            Ok(GossipCheckOk::Admitted)
        ));
    }

    #[test]
    fn test_multiple_gossipers_tracked() {
        let mut intake = GossipIntake::new(&GossipConfig::default());

        // Several gossipers each admit a handful of records; none should be
        // rate limited and the LRU must keep their budgets separate.
        for g in 0..10u8 {
            let source = gossiper(g + 100);
            for i in 0..5u8 {
                let n = g * 10 + i + 20;
                let peer_id = test_peer_id(n);
                let mut overlay_bytes = [0u8; 32];
                overlay_bytes[0] = n;
                let peer =
                    mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);
                assert!(matches!(
                    intake.check_gossip(&peer, &source, None),
                    Ok(GossipCheckOk::Admitted)
                ));
            }
        }
    }
}
