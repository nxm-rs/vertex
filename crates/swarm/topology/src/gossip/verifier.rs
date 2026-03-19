//! Secure gossip verification for received peers.
//!
//! All peers received via gossip MUST be verified via handshake before
//! being stored in the peer manager. This prevents storage exhaustion attacks
//! and ensures only cryptographically valid peers enter our peer store.

use std::{sync::LazyLock, time::Duration};

use hashlink::LruCache;
use libp2p::PeerId;
use metrics::gauge;
use tracing::{debug, warn};
use vertex_net_dialer::{DialRequest, DialTracker, DialTrackerConfig, EnqueueError};
use vertex_swarm_peer::{SwarmAddress, SwarmPeer};

use super::error::{GossipCheckError, VerificationFailure};
use super::events::{GossipCheckOk, VerificationResult};
use crate::extract_peer_id;

type OverlayAddress = SwarmAddress;

/// Fallback dial address used when the gossiped peer has no multiaddrs.
/// This should never happen — `check_gossip` validates non-empty addrs.
#[allow(clippy::unwrap_used)]
static FALLBACK_ADDR: LazyLock<libp2p::Multiaddr> =
    LazyLock::new(|| "/ip4/0.0.0.0/tcp/0".parse().unwrap());

const MAX_PENDING_PER_GOSSIPER: usize = 64;
/// ~32 concurrent connections x ~30 gossiped peers each = 960, rounded to 1024.
const MAX_TOTAL_PENDING: usize = 1024;
const PENDING_EXPIRY: Duration = Duration::from_secs(60);
const MAX_CONCURRENT_VERIFICATIONS: usize = 32;
const MAX_TRACKED_GOSSIPERS: usize = 1024;
/// Interval between expired entry cleanup runs.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(10);

/// Backoff/ban configuration for unreachable gossiped peers.
const BACKOFF_CAPACITY: usize = 1024;
const BACKOFF_BASE_SECS: u64 = 5;
const BACKOFF_MAX_SECS: u64 = 20;
const BAN_CAPACITY: usize = 8192;
const BAN_AFTER_FAILURES: u32 = 3;
const BAN_TTL_SECS: u64 = 3600;

/// Data carried with each verification dial request.
#[derive(Debug)]
struct VerificationData {
    /// The gossiped peer data (unverified).
    gossiped_peer: SwarmPeer,
    /// Who gossiped this peer to us (for scoring on result).
    gossiper: OverlayAddress,
}

/// A gossip verification at different lifecycle stages.
pub(super) enum Verification {
    /// Ready to dial -- returned by `next_verification_dial`.
    Pending {
        peer_id: PeerId,
        addrs: Vec<libp2p::Multiaddr>,
    },
    /// Dial completed -- returned by `resolve_in_flight`.
    Resolved {
        /// The gossiped peer data (unverified).
        gossiped_peer: SwarmPeer,
        /// Who gossiped this peer to us (for scoring on result).
        gossiper: OverlayAddress,
        /// The address we dialed.
        dial_addr: libp2p::Multiaddr,
    },
}

/// Manages pending gossip verifications.
///
/// Verification dials are isolated from the connection registry.
/// All verification state is tracked internally with bounded memory usage.
/// Delegates queue/in-flight tracking to `DialTracker`.
pub(super) struct GossipVerifier {
    /// Per-gossiper pending count for rate limiting (LRU-bounded).
    per_gossiper_count: LruCache<OverlayAddress, usize>,
    dial_tracker: DialTracker<OverlayAddress, VerificationData>,
}

impl GossipVerifier {
    pub(super) fn new() -> Self {
        let config = DialTrackerConfig {
            max_pending: MAX_TOTAL_PENDING,
            max_in_flight: MAX_CONCURRENT_VERIFICATIONS,
            pending_ttl: PENDING_EXPIRY,
            in_flight_timeout: PENDING_EXPIRY,
            cleanup_interval: CLEANUP_INTERVAL,
            metrics_label: Some("gossip"),
            backoff_capacity: BACKOFF_CAPACITY,
            backoff_base_secs: BACKOFF_BASE_SECS,
            backoff_max_secs: BACKOFF_MAX_SECS,
            ban_capacity: BAN_CAPACITY,
            ban_after_failures: BAN_AFTER_FAILURES,
            ban_ttl_secs: BAN_TTL_SECS,
        };
        Self {
            per_gossiper_count: LruCache::new(MAX_TRACKED_GOSSIPERS),
            dial_tracker: DialTracker::new(config),
        }
    }

    /// Check a gossiped peer and determine if it needs verification.
    pub(super) fn check_gossip(
        &mut self,
        gossiped_peer: &SwarmPeer,
        gossiper: &OverlayAddress,
        existing_peer: Option<&SwarmPeer>,
    ) -> Result<GossipCheckOk, GossipCheckError> {
        if gossiped_peer.multiaddrs().is_empty() {
            return Err(GossipCheckError::NoMultiaddrs);
        }

        let Some(peer_id) = extract_peer_id(&gossiped_peer.multiaddrs()[0]) else {
            return Err(GossipCheckError::NoPeerId);
        };

        let overlay = gossiped_peer.overlay();

        if self.dial_tracker.contains_id(overlay) || self.dial_tracker.contains_peer(&peer_id) {
            return Err(EnqueueError::AlreadyPending.into());
        }

        if let Some(existing) = existing_peer
            && Self::signatures_match(existing, gossiped_peer)
            && Self::has_matching_multiaddr(existing, gossiped_peer)
        {
            return Ok(GossipCheckOk::AlreadyKnown);
        }

        let gossiper_count = self.per_gossiper_count.get(gossiper).copied().unwrap_or(0);
        if gossiper_count >= MAX_PENDING_PER_GOSSIPER {
            return Err(GossipCheckError::GossiperRateLimited);
        }

        let request = DialRequest::new(
            *overlay,
            peer_id,
            gossiped_peer.multiaddrs().to_vec(),
            VerificationData {
                gossiped_peer: gossiped_peer.clone(),
                gossiper: *gossiper,
            },
        );

        self.dial_tracker.enqueue(request)?;

        let count = self.per_gossiper_count.get(gossiper).copied().unwrap_or(0);
        self.per_gossiper_count.insert(*gossiper, count + 1);
        gauge!("topology_gossip_tracked_gossipers").set(self.per_gossiper_count.len() as f64);
        Ok(GossipCheckOk::Enqueued)
    }

    /// Get next peer to dial for verification, respecting concurrency limits.
    pub(super) fn next_verification_dial(&mut self) -> Option<Verification> {
        self.next_verification_batch(1).into_iter().next()
    }

    /// Get a batch of peers to dial for verification, respecting concurrency limits.
    pub(super) fn next_verification_batch(&mut self, max_batch: usize) -> Vec<Verification> {
        self.dial_tracker
            .next_batch(max_batch)
            .into_iter()
            .map(|dispatch| Verification::Pending {
                peer_id: dispatch.peer_id,
                addrs: dispatch.addrs,
            })
            .collect()
    }

    /// Resolve an in-flight verification by PeerId.
    ///
    /// Removes the verification from in-flight tracking and
    /// decrements the gossiper count. Returns the verification data
    /// if one existed for this peer.
    pub(super) fn resolve_in_flight(&mut self, peer_id: &PeerId) -> Option<Verification> {
        let request = self.dial_tracker.resolve(peer_id)?;

        // Decrement per-gossiper count, removing entry if it hits zero.
        if let Some(count) = self.per_gossiper_count.get_mut(&request.data.gossiper) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.per_gossiper_count.remove(&request.data.gossiper);
            }
        }
        gauge!("topology_gossip_tracked_gossipers").set(self.per_gossiper_count.len() as f64);
        Some(Verification::Resolved {
            gossiped_peer: request.data.gossiped_peer,
            gossiper: request.data.gossiper,
            dial_addr: request
                .addrs
                .into_iter()
                .next()
                // Should never happen — check_gossip validates non-empty multiaddrs
                .unwrap_or_else(|| FALLBACK_ADDR.clone()),
        })
    }

    /// Record a backoff for an unreachable gossiped peer.
    pub(super) fn record_backoff(&mut self, id: &OverlayAddress) {
        self.dial_tracker.record_backoff(id);
    }

    /// Clear backoff/ban state after successful verification.
    pub(super) fn clear_backoff(&mut self, id: &OverlayAddress) {
        self.dial_tracker.clear_backoff(id);
    }

    /// Compare a verified peer from handshake against the original gossiped data.
    ///
    /// Consumes the resolved fields and returns the gossiper address
    /// alongside the result so the caller can attribute the outcome.
    pub(super) fn verify_handshake(
        gossiped_peer: SwarmPeer,
        gossiper: OverlayAddress,
        dial_addr: libp2p::Multiaddr,
        verified_peer: SwarmPeer,
    ) -> (OverlayAddress, VerificationResult) {
        let gossiped_overlay = *gossiped_peer.overlay();
        let verified_overlay = *verified_peer.overlay();

        // CASE 1: Overlays match
        if verified_overlay == gossiped_overlay {
            // Verify multiaddr containment
            if !verified_peer
                .multiaddrs()
                .iter()
                .any(|addr| addr == &dial_addr)
            {
                warn!(
                    overlay = %verified_overlay,
                    gossiped_addr = %dial_addr,
                    verified_addrs = ?verified_peer.multiaddrs(),
                    %gossiper,
                    "Gossip verification failed: gossiped multiaddr not in verified peer"
                );
                return (
                    gossiper,
                    VerificationResult::Failed {
                        reason: VerificationFailure::MultiAddrNotInPeer,
                    },
                );
            }

            // Compare signatures (byte-level)
            if Self::signatures_match(&gossiped_peer, &verified_peer) {
                debug!(
                    overlay = %verified_overlay,
                    %gossiper,
                    "Gossip verification successful"
                );
                return (gossiper, VerificationResult::Verified { verified_peer });
            }

            // Same overlay, different signature -- identity rotation
            debug!(
                overlay = %verified_overlay,
                %gossiper,
                "Gossip verification: identity updated (same overlay, different signature)"
            );
            return (
                gossiper,
                VerificationResult::IdentityUpdated { verified_peer },
            );
        }

        // CASE 2: Different overlay -- completely different peer at this address
        warn!(
            verified = %verified_overlay,
            gossiped = %gossiped_overlay,
            %gossiper,
            "Wrong overlay gossiped - different peer at address"
        );
        (
            gossiper,
            VerificationResult::DifferentPeerAtAddress {
                verified_peer,
                gossiped_overlay,
            },
        )
    }

    /// Check if two peers have matching signatures.
    fn signatures_match(a: &SwarmPeer, b: &SwarmPeer) -> bool {
        a.signature() == b.signature()
    }

    /// Check if the gossiped peer's multiaddr is in the existing peer's list.
    fn has_matching_multiaddr(existing: &SwarmPeer, gossiped: &SwarmPeer) -> bool {
        // Check if at least one gossiped addr is in existing's list
        gossiped
            .multiaddrs()
            .iter()
            .any(|addr| existing.multiaddrs().contains(addr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, Signature, U256};
    use vertex_swarm_test_utils::test_peer_id;

    fn mock_swarm_peer_with_peer_id(
        overlay_bytes: [u8; 32],
        addr: &str,
        peer_id: PeerId,
    ) -> SwarmPeer {
        let addr_with_p2p = format!("{}/p2p/{}", addr, peer_id);
        let multiaddr: libp2p::Multiaddr = addr_with_p2p.parse().unwrap();
        SwarmPeer::from_validated(
            vec![multiaddr],
            Signature::new(U256::from(1u64), U256::from(2u64), false),
            B256::from(overlay_bytes),
            B256::ZERO,
            Address::ZERO,
        )
    }

    fn mock_swarm_peer_with_signature(
        overlay_bytes: [u8; 32],
        addr: &str,
        peer_id: PeerId,
        sig_r: u64,
        sig_s: u64,
    ) -> SwarmPeer {
        let addr_with_p2p = format!("{}/p2p/{}", addr, peer_id);
        let multiaddr: libp2p::Multiaddr = addr_with_p2p.parse().unwrap();
        SwarmPeer::from_validated(
            vec![multiaddr],
            Signature::new(U256::from(sig_r), U256::from(sig_s), false),
            B256::from(overlay_bytes),
            B256::ZERO,
            Address::ZERO,
        )
    }

    fn mock_swarm_peer_no_p2p(overlay_bytes: [u8; 32], addr: &str) -> SwarmPeer {
        let multiaddr: libp2p::Multiaddr = addr.parse().unwrap();
        SwarmPeer::from_validated(
            vec![multiaddr],
            Signature::new(U256::from(1u64), U256::from(2u64), false),
            B256::from(overlay_bytes),
            B256::ZERO,
            Address::ZERO,
        )
    }

    #[test]
    fn test_rejects_no_peer_id() {
        let mut verifier = GossipVerifier::new();

        // Multiaddr without /p2p/ component
        let peer = mock_swarm_peer_no_p2p([2u8; 32], "/ip4/1.2.3.4/tcp/1634");
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        let result = verifier.check_gossip(&peer, &gossiper, None);
        assert!(matches!(result, Err(GossipCheckError::NoPeerId)));
    }

    #[test]
    fn test_already_known_with_matching_signature() {
        let mut verifier = GossipVerifier::new();

        let peer_id = test_peer_id(2);
        let peer = mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        let existing = peer.clone();
        let result = verifier.check_gossip(&peer, &gossiper, Some(&existing));
        assert!(matches!(result, Ok(GossipCheckOk::AlreadyKnown)));
    }

    #[test]
    fn test_needs_verification_for_new_peer() {
        let mut verifier = GossipVerifier::new();

        let peer_id = test_peer_id(2);
        let peer = mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        let result = verifier.check_gossip(&peer, &gossiper, None);
        assert!(matches!(result, Ok(GossipCheckOk::Enqueued)));
        assert_eq!(verifier.dial_tracker.pending_count(), 1);
    }

    #[test]
    fn test_rate_limit_per_gossiper() {
        let mut verifier = GossipVerifier::new();
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        // Add MAX_PENDING_PER_GOSSIPER peers
        for i in 0..MAX_PENDING_PER_GOSSIPER {
            let peer_id = test_peer_id((i + 10) as u8);
            let mut overlay_bytes = [0u8; 32];
            overlay_bytes[0] = (i + 10) as u8;
            let peer =
                mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);
            let result = verifier.check_gossip(&peer, &gossiper, None);
            assert!(matches!(result, Ok(GossipCheckOk::Enqueued)));
        }

        // Next should be rate limited
        let peer_id = test_peer_id(100);
        let peer = mock_swarm_peer_with_peer_id([100u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let result = verifier.check_gossip(&peer, &gossiper, None);
        assert!(matches!(result, Err(GossipCheckError::GossiperRateLimited)));
    }

    #[test]
    fn test_verification_success() {
        let mut verifier = GossipVerifier::new();

        let peer_id = test_peer_id(2);
        let peer = mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        verifier.check_gossip(&peer, &gossiper, None).unwrap();

        // Simulate dial and handshake
        let _dial = verifier.next_verification_dial().unwrap();
        let Verification::Resolved {
            gossiped_peer,
            gossiper,
            dial_addr,
        } = verifier.resolve_in_flight(&peer_id).unwrap()
        else {
            return;
        };
        let (_gossiper, result) =
            GossipVerifier::verify_handshake(gossiped_peer, gossiper, dial_addr, peer);

        assert!(matches!(result, VerificationResult::Verified { .. }));
        assert_eq!(verifier.dial_tracker.pending_count(), 0);
    }

    #[test]
    fn test_verification_identity_updated() {
        let mut verifier = GossipVerifier::new();

        let peer_id = test_peer_id(2);
        // Gossiped peer with signature (1, 2)
        let gossiped_peer =
            mock_swarm_peer_with_signature([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id, 1, 2);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        verifier
            .check_gossip(&gossiped_peer, &gossiper, None)
            .unwrap();
        let _dial = verifier.next_verification_dial().unwrap();

        // Verified peer has same overlay but different signature (3, 4)
        let verified_peer =
            mock_swarm_peer_with_signature([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id, 3, 4);
        let Verification::Resolved {
            gossiped_peer,
            gossiper,
            dial_addr,
        } = verifier.resolve_in_flight(&peer_id).unwrap()
        else {
            return;
        };
        let (_gossiper, result) =
            GossipVerifier::verify_handshake(gossiped_peer, gossiper, dial_addr, verified_peer);

        assert!(matches!(result, VerificationResult::IdentityUpdated { .. }));
    }

    #[test]
    fn test_verification_different_peer_at_address() {
        let mut verifier = GossipVerifier::new();

        let peer_id = test_peer_id(2);
        let gossiped_peer =
            mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));
        let gossiped_overlay = *gossiped_peer.overlay();

        verifier
            .check_gossip(&gossiped_peer, &gossiper, None)
            .unwrap();
        let _dial = verifier.next_verification_dial().unwrap();

        // Handshake returns completely different overlay
        let verified_peer =
            mock_swarm_peer_with_peer_id([99u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let Verification::Resolved {
            gossiped_peer,
            gossiper,
            dial_addr,
        } = verifier.resolve_in_flight(&peer_id).unwrap()
        else {
            return;
        };
        let (_gossiper, result) =
            GossipVerifier::verify_handshake(gossiped_peer, gossiper, dial_addr, verified_peer);

        match result {
            VerificationResult::DifferentPeerAtAddress {
                gossiped_overlay: returned_gossiped,
                ..
            } => {
                assert_eq!(returned_gossiped, gossiped_overlay);
            }
            _ => panic!("Expected DifferentPeerAtAddress, got {:?}", result),
        }
    }

    #[test]
    fn test_is_in_flight() {
        let mut verifier = GossipVerifier::new();

        let peer_id = test_peer_id(2);
        let peer = mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        verifier.check_gossip(&peer, &gossiper, None).unwrap();
        assert!(!verifier.dial_tracker.is_in_flight(&peer_id));

        let Verification::Pending {
            peer_id: dial_peer_id,
            ..
        } = verifier.next_verification_dial().unwrap()
        else {
            return;
        };
        assert!(verifier.dial_tracker.is_in_flight(&dial_peer_id));
    }

    #[test]
    fn test_resolve_in_flight_on_dial_failure() {
        let mut verifier = GossipVerifier::new();

        let peer_id = test_peer_id(2);
        let peer = mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        verifier.check_gossip(&peer, &gossiper, None).unwrap();
        let _dial = verifier.next_verification_dial().unwrap();

        // resolve_in_flight cleans up accounting; task decides the result
        let Verification::Resolved {
            gossiper: resolved_gossiper,
            ..
        } = verifier.resolve_in_flight(&peer_id).unwrap()
        else {
            return;
        };
        assert_eq!(resolved_gossiper, gossiper);
        assert_eq!(verifier.dial_tracker.pending_count(), 0);
    }

    #[test]
    fn test_total_pending_limit() {
        let mut verifier = GossipVerifier::new();

        // Use many gossipers to avoid per-gossiper rate limit.
        // PeerId::random() ensures unique peer IDs beyond the u8 limit of test_peer_id.
        for i in 0..MAX_TOTAL_PENDING {
            let peer_id = PeerId::random();
            let mut overlay_bytes = [0u8; 32];
            overlay_bytes[0] = (i % 256) as u8;
            overlay_bytes[1] = (i / 256) as u8;
            overlay_bytes[2] = (i / 65536) as u8;
            let peer =
                mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);

            // Each peer from a different gossiper
            let mut gossiper_bytes = [0u8; 32];
            gossiper_bytes[0] = ((i + 100) % 256) as u8;
            gossiper_bytes[1] = ((i + 100) / 256) as u8;
            gossiper_bytes[2] = ((i + 100) / 65536) as u8;
            let gossiper = OverlayAddress::from(B256::from(gossiper_bytes));

            let result = verifier.check_gossip(&peer, &gossiper, None);
            assert!(
                result.is_ok(),
                "Expected Ok for peer {}, got {:?}",
                i,
                result
            );
        }

        assert_eq!(verifier.dial_tracker.pending_count(), MAX_TOTAL_PENDING);

        // Next should be rejected with QueueFull
        let peer_id = PeerId::random();
        let peer = mock_swarm_peer_with_peer_id([250u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([251u8; 32]));
        let result = verifier.check_gossip(&peer, &gossiper, None);
        assert!(matches!(
            result,
            Err(GossipCheckError::Enqueue(EnqueueError::QueueFull))
        ));
    }

    #[test]
    fn test_multiple_gossipers_tracked() {
        let mut verifier = GossipVerifier::new();

        // Track multiple gossipers to verify LRU doesn't break rate limiting
        for g in 0..10 {
            let gossiper = OverlayAddress::from(B256::from([(g + 100) as u8; 32]));

            for i in 0..5 {
                let peer_id = test_peer_id((g * 10 + i + 20) as u8);
                let mut overlay_bytes = [0u8; 32];
                overlay_bytes[0] = (g * 10 + i + 20) as u8;
                let peer =
                    mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);

                let result = verifier.check_gossip(&peer, &gossiper, None);
                assert!(matches!(result, Ok(GossipCheckOk::Enqueued)));
            }
        }

        // 10 gossipers * 5 peers each = 50 pending
        assert_eq!(verifier.dial_tracker.pending_count(), 50);
    }

    #[test]
    fn test_concurrent_verifications_limit() {
        let mut verifier = GossipVerifier::new();

        // Add more peers than MAX_CONCURRENT_VERIFICATIONS
        for i in 0..MAX_CONCURRENT_VERIFICATIONS + 5 {
            let peer_id = test_peer_id((i + 10) as u8);
            let mut overlay_bytes = [0u8; 32];
            overlay_bytes[0] = (i + 10) as u8;
            let peer =
                mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);
            let gossiper = OverlayAddress::from(B256::from([3u8; 32]));
            verifier.check_gossip(&peer, &gossiper, None).unwrap();
        }

        // Start MAX_CONCURRENT_VERIFICATIONS dials
        for _ in 0..MAX_CONCURRENT_VERIFICATIONS {
            assert!(verifier.next_verification_dial().is_some());
        }

        // Next should return None (at limit)
        assert!(verifier.next_verification_dial().is_none());
    }

    #[test]
    fn test_verification_batch() {
        let mut verifier = GossipVerifier::new();

        // Add 10 peers
        for i in 0..10 {
            let peer_id = test_peer_id((i + 10) as u8);
            let mut overlay_bytes = [0u8; 32];
            overlay_bytes[0] = (i + 10) as u8;
            let peer =
                mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);
            let gossiper = OverlayAddress::from(B256::from([3u8; 32]));
            verifier.check_gossip(&peer, &gossiper, None).unwrap();
        }

        // Get batch of 5
        let batch = verifier.next_verification_batch(5);
        assert_eq!(batch.len(), 5);

        // All should be in-flight now
        for v in &batch {
            let Verification::Pending { peer_id, .. } = v else {
                continue;
            };
            assert!(verifier.dial_tracker.is_in_flight(peer_id));
        }

        // Get another batch (should get remaining 5)
        let batch2 = verifier.next_verification_batch(10);
        assert_eq!(batch2.len(), 5);

        // No more available
        let batch3 = verifier.next_verification_batch(10);
        assert!(batch3.is_empty());
    }

    #[test]
    fn test_backoff_rejects_different_gossiper() {
        let mut verifier = GossipVerifier::new();

        let peer_id = test_peer_id(2);
        let overlay_bytes = [2u8; 32];
        let overlay = OverlayAddress::from(B256::from(overlay_bytes));

        // Record a backoff for this overlay
        verifier.record_backoff(&overlay);

        // Different gossiper tries to gossip the same peer
        let gossiper2 = OverlayAddress::from(B256::from([99u8; 32]));
        let peer = mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);

        let result = verifier.check_gossip(&peer, &gossiper2, None);
        assert!(matches!(
            result,
            Err(GossipCheckError::Enqueue(EnqueueError::InBackoff))
        ));
    }

    #[test]
    fn test_clear_backoff_allows_re_verification() {
        let mut verifier = GossipVerifier::new();

        let peer_id = test_peer_id(2);
        let overlay_bytes = [2u8; 32];
        let overlay = OverlayAddress::from(B256::from(overlay_bytes));

        // Ban the overlay (3 failures)
        verifier.record_backoff(&overlay);
        verifier.record_backoff(&overlay);
        verifier.record_backoff(&overlay);

        // Should be banned
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));
        let peer = mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);
        assert!(matches!(
            verifier.check_gossip(&peer, &gossiper, None),
            Err(GossipCheckError::Enqueue(EnqueueError::Banned))
        ));

        // Clear and re-enqueue
        verifier.clear_backoff(&overlay);
        assert!(matches!(
            verifier.check_gossip(&peer, &gossiper, None),
            Ok(GossipCheckOk::Enqueued)
        ));
    }
}
