//! Secure gossip verification for hive-received peers.
//!
//! All peers received via Hive gossip MUST be verified via handshake before
//! being stored in the peer manager. This prevents storage exhaustion attacks
//! and ensures only cryptographically valid peers enter our peer store.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use hashlink::{LinkedHashMap, LruCache};
use libp2p::{Multiaddr, PeerId};
use tracing::{debug, trace, warn};
use vertex_swarm_peer::{SwarmAddress, SwarmPeer};

use crate::extract_peer_id;

pub(crate) type OverlayAddress = SwarmAddress;

const MAX_PENDING_PER_GOSSIPER: usize = 64;
const MAX_TOTAL_PENDING: usize = 2048;
const PENDING_EXPIRY: Duration = Duration::from_secs(60);
const MAX_CONCURRENT_VERIFICATIONS: usize = 32;
const MAX_TRACKED_GOSSIPERS: usize = 1024;
/// Interval between expired entry cleanup runs.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(10);

/// A peer pending verification via handshake.
#[derive(Debug, Clone)]
pub(crate) struct PendingVerification {
    /// The gossiped peer data (unverified).
    pub gossiped_peer: SwarmPeer,
    /// Who gossiped this peer to us (for scoring on result).
    pub gossiper: OverlayAddress,
    /// When this was queued.
    pub queued_at: Instant,
    /// PeerId extracted from first multiaddr for tracking in-flight dials.
    pub peer_id: PeerId,
}

/// In-flight verification dial state.
#[derive(Debug)]
struct InFlightVerification {
    /// The gossiped overlay we're verifying.
    gossiped_overlay: OverlayAddress,
    /// When the dial started.
    started_at: Instant,
}

impl PendingVerification {
    /// Check if this pending verification has expired.
    fn is_expired(&self) -> bool {
        self.queued_at.elapsed() > PENDING_EXPIRY
    }

    /// Get the dial address (first multiaddr from gossiped peer).
    pub(crate) fn dial_addr(&self) -> &Multiaddr {
        // Safe: check_gossip validates multiaddrs is non-empty before creating PendingVerification
        &self.gossiped_peer.multiaddrs()[0]
    }
}

/// Result of checking a gossiped peer against existing data.
#[derive(Debug)]
pub(crate) enum GossipCheckResult {
    /// Peer already exists with matching signature - skip verification.
    AlreadyKnown,
    /// Peer is new or has different data - needs verification.
    NeedsVerification(PendingVerification),
    /// Peer was rejected (self-overlay, invalid format, etc.).
    Rejected(GossipRejectionReason),
}

/// Reasons for rejecting gossiped peers before verification.
#[derive(Debug, Clone, Copy)]
pub(crate) enum GossipRejectionReason {
    /// Gossiped our own overlay address.
    SelfOverlay,
    /// No multiaddrs in gossip.
    NoMultiaddrs,
    /// No /p2p/ component in multiaddr (cannot extract PeerId).
    NoPeerId,
    /// Gossiper is sending too many peers.
    GossiperRateLimited,
    /// Total pending queue is full.
    QueueFull,
    /// Already pending verification for this overlay.
    AlreadyPending,
}

/// Result of verifying a gossiped peer against handshake data.
#[derive(Debug)]
pub(crate) enum VerificationResult {
    /// Signatures match - fully verified.
    Verified {
        /// The verified peer from handshake (authoritative).
        verified_peer: SwarmPeer,
        /// Who gossiped this peer (for positive scoring).
        gossiper: OverlayAddress,
    },
    /// Same overlay, different signature - identity rotation.
    IdentityUpdated {
        /// The verified peer from handshake (authoritative).
        verified_peer: SwarmPeer,
        /// Who gossiped this peer (for positive scoring).
        gossiper: OverlayAddress,
    },
    /// Different overlay - wrong gossip info, but real peer discovered.
    DifferentPeerAtAddress {
        /// The verified peer from handshake (authoritative).
        verified_peer: SwarmPeer,
        /// The overlay that was gossiped (incorrect).
        gossiped_overlay: OverlayAddress,
        /// Who gossiped the incorrect peer.
        gossiper: OverlayAddress,
    },
    /// Verification failed - penalize gossiper.
    Failed {
        /// Who gossiped the invalid peer.
        gossiper: OverlayAddress,
        /// Why verification failed.
        reason: VerificationFailureReason,
    },
    /// Peer was unreachable (dial failed).
    Unreachable {
        /// Who gossiped the unreachable peer.
        gossiper: OverlayAddress,
    },
    /// No pending verification for this peer (unexpected handshake).
    NotPending,
}

/// Reasons why verification can fail.
#[derive(Debug, Clone, Copy)]
pub(crate) enum VerificationFailureReason {
    /// The gossiped multiaddr is not in the verified peer's address list.
    MultiAddrNotInPeer,
}

/// Statistics for metrics reporting.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GossipVerifierStats {
    /// Number of peers pending verification.
    pub pending_count: usize,
    /// Number of verification dials currently in-flight.
    pub in_flight_count: usize,
    /// Number of distinct gossipers being tracked for rate limiting.
    pub tracked_gossipers: usize,
    /// Estimated memory usage in bytes.
    pub estimated_memory_bytes: usize,
}

/// Manages pending gossip verifications.
///
/// Verification dials are isolated from the connection registry.
/// All verification state is tracked internally with bounded memory usage.
pub(crate) struct GossipVerifier {
    /// Pending verifications in insertion order (FIFO dial selection).
    pending: LinkedHashMap<OverlayAddress, PendingVerification>,
    in_flight: HashMap<PeerId, InFlightVerification>,
    /// O(1) lookup for overlays currently being verified.
    in_flight_overlays: HashSet<OverlayAddress>,
    /// Per-gossiper pending count for rate limiting (LRU-bounded).
    per_gossiper_count: LruCache<OverlayAddress, usize>,
    local_overlay: OverlayAddress,
    last_cleanup: Instant,
}

impl GossipVerifier {
    pub(crate) fn new(local_overlay: OverlayAddress) -> Self {
        Self {
            pending: LinkedHashMap::new(),
            in_flight: HashMap::new(),
            in_flight_overlays: HashSet::new(),
            per_gossiper_count: LruCache::new(MAX_TRACKED_GOSSIPERS),
            local_overlay,
            last_cleanup: Instant::now(),
        }
    }

    /// Check if a PeerId is currently an in-flight verification dial.
    pub(crate) fn is_in_flight(&self, peer_id: &PeerId) -> bool {
        self.in_flight.contains_key(peer_id)
    }

    /// Check a gossiped peer and determine if it needs verification.
    ///
    /// Returns what action to take for this peer.
    pub(crate) fn check_gossip(
        &mut self,
        gossiped_peer: SwarmPeer,
        gossiper: OverlayAddress,
        existing_peer: Option<&SwarmPeer>,
    ) -> GossipCheckResult {
        let overlay = OverlayAddress::from(*gossiped_peer.overlay());

        // Reject self-overlay
        if overlay == self.local_overlay {
            trace!("Rejecting self-overlay from gossip");
            return GossipCheckResult::Rejected(GossipRejectionReason::SelfOverlay);
        }

        // Reject if no multiaddrs
        if gossiped_peer.multiaddrs().is_empty() {
            trace!(%overlay, "Rejecting peer with no multiaddrs");
            return GossipCheckResult::Rejected(GossipRejectionReason::NoMultiaddrs);
        }

        // Extract PeerId from first multiaddr - required for tracking
        let Some(peer_id) = extract_peer_id(&gossiped_peer.multiaddrs()[0]) else {
            trace!(%overlay, "Rejecting peer with no /p2p/ in multiaddr");
            return GossipCheckResult::Rejected(GossipRejectionReason::NoPeerId);
        };

        // Check if already pending (by overlay or by peer_id in-flight)
        if self.pending.contains_key(&overlay) {
            trace!(%overlay, "Peer already pending verification");
            return GossipCheckResult::Rejected(GossipRejectionReason::AlreadyPending);
        }
        if self.in_flight.contains_key(&peer_id) {
            trace!(%overlay, %peer_id, "PeerId already in-flight for verification");
            return GossipCheckResult::Rejected(GossipRejectionReason::AlreadyPending);
        }

        // Check if peer already exists with matching signature
        if let Some(existing) = existing_peer {
            if Self::signatures_match(existing, &gossiped_peer)
                && Self::has_matching_multiaddr(existing, &gossiped_peer)
            {
                trace!(%overlay, "Peer already known with matching signature");
                return GossipCheckResult::AlreadyKnown;
            }
            // Different signature or multiaddrs - needs re-verification
            debug!(%overlay, "Existing peer has different signature/addrs, will re-verify");
        }

        // Rate limit per gossiper
        let gossiper_count = self.per_gossiper_count.get(&gossiper).copied().unwrap_or(0);
        if gossiper_count >= MAX_PENDING_PER_GOSSIPER {
            debug!(%gossiper, "Gossiper rate limited");
            return GossipCheckResult::Rejected(GossipRejectionReason::GossiperRateLimited);
        }

        // Check total queue size
        if self.pending.len() >= MAX_TOTAL_PENDING {
            debug!("Verification queue full");
            return GossipCheckResult::Rejected(GossipRejectionReason::QueueFull);
        }

        let pending = PendingVerification {
            gossiped_peer,
            gossiper,
            queued_at: Instant::now(),
            peer_id,
        };

        // Add to pending (LinkedHashMap maintains insertion order for FIFO)
        self.pending.insert(overlay, pending.clone());
        let count = self.per_gossiper_count.get(&gossiper).copied().unwrap_or(0);
        self.per_gossiper_count.insert(gossiper, count + 1);

        GossipCheckResult::NeedsVerification(pending)
    }

    /// Get next peer to dial for verification, respecting concurrency limits.
    pub(crate) fn next_verification_dial(&mut self) -> Option<PendingVerification> {
        self.next_verification_batch(1).into_iter().next()
    }

    /// Get a batch of peers to dial for verification, respecting concurrency limits.
    ///
    /// Returns up to `max_batch` pending verifications that aren't already in-flight.
    pub(crate) fn next_verification_batch(&mut self, max_batch: usize) -> Vec<PendingVerification> {
        // Periodic cleanup (not every call)
        if self.last_cleanup.elapsed() > CLEANUP_INTERVAL {
            self.in_flight.retain(|_, v| {
                let keep = v.started_at.elapsed() < PENDING_EXPIRY;
                if !keep {
                    self.in_flight_overlays.remove(&v.gossiped_overlay);
                }
                keep
            });
            self.cleanup_expired_pending();
            self.last_cleanup = Instant::now();
        }

        let available_slots = MAX_CONCURRENT_VERIFICATIONS.saturating_sub(self.in_flight.len());
        let batch_size = max_batch.min(available_slots);

        if batch_size == 0 {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(batch_size);
        let mut expired = Vec::new();

        // Find eligible pending entries (FIFO via LinkedHashMap iteration)
        for (overlay, pending) in self.pending.iter() {
            if result.len() >= batch_size {
                break;
            }

            // Skip if already in-flight for this overlay (O(1) lookup)
            if self.in_flight_overlays.contains(overlay) {
                continue;
            }

            // Check expiry
            if pending.is_expired() {
                expired.push(*overlay);
                continue;
            }

            result.push((*overlay, pending.clone()));
        }

        // Clean up expired entries
        for overlay in expired {
            self.remove_pending(&overlay);
        }

        // Mark all as in-flight
        for (overlay, pending) in &result {
            self.in_flight.insert(pending.peer_id, InFlightVerification {
                gossiped_overlay: *overlay,
                started_at: Instant::now(),
            });
            self.in_flight_overlays.insert(*overlay);
        }

        result.into_iter().map(|(_, p)| p).collect()
    }

    fn cleanup_expired_pending(&mut self) {
        let expired: Vec<OverlayAddress> = self
            .pending
            .iter()
            .filter(|(_, p)| p.is_expired())
            .map(|(o, _)| *o)
            .collect();

        for overlay in expired {
            self.remove_pending(&overlay);
        }
    }

    /// Called when handshake completes for a verification dial.
    ///
    /// Compares the handshake result against the gossiped data.
    pub(crate) fn on_verification_handshake(
        &mut self,
        peer_id: PeerId,
        verified_peer: SwarmPeer,
    ) -> VerificationResult {
        // Get in-flight state by PeerId
        let Some(in_flight) = self.in_flight.remove(&peer_id) else {
            return VerificationResult::NotPending;
        };

        let gossiped_overlay = in_flight.gossiped_overlay;
        self.in_flight_overlays.remove(&gossiped_overlay);

        // Get the pending verification by gossiped overlay
        let Some(pending) = self.pending.remove(&gossiped_overlay) else {
            return VerificationResult::NotPending;
        };

        self.decrement_gossiper_count(&pending.gossiper);

        let verified_overlay = OverlayAddress::from(*verified_peer.overlay());
        let gossiper = pending.gossiper;

        // CASE 1: Overlays match
        if verified_overlay == gossiped_overlay {
            // Verify multiaddr containment
            if !verified_peer
                .multiaddrs()
                .iter()
                .any(|addr| addr == pending.dial_addr())
            {
                warn!(
                    overlay = %verified_overlay,
                    gossiped_addr = %pending.dial_addr(),
                    verified_addrs = ?verified_peer.multiaddrs(),
                    %gossiper,
                    "Gossip verification failed: gossiped multiaddr not in verified peer"
                );
                return VerificationResult::Failed {
                    gossiper,
                    reason: VerificationFailureReason::MultiAddrNotInPeer,
                };
            }

            // Compare signatures (byte-level)
            if Self::signatures_match(&pending.gossiped_peer, &verified_peer) {
                debug!(
                    overlay = %verified_overlay,
                    %gossiper,
                    "Gossip verification successful"
                );
                return VerificationResult::Verified { verified_peer, gossiper };
            }

            // Same overlay, different signature - identity rotation
            debug!(
                overlay = %verified_overlay,
                %gossiper,
                "Gossip verification: identity updated (same overlay, different signature)"
            );
            return VerificationResult::IdentityUpdated { verified_peer, gossiper };
        }

        // CASE 2: Different overlay - completely different peer at this address
        warn!(
            verified = %verified_overlay,
            gossiped = %gossiped_overlay,
            %gossiper,
            "Wrong overlay gossiped - different peer at address"
        );
        VerificationResult::DifferentPeerAtAddress {
            verified_peer,
            gossiped_overlay,
            gossiper,
        }
    }

    /// Called when a verification dial fails.
    pub(crate) fn on_verification_dial_failed(&mut self, peer_id: &PeerId) -> VerificationResult {
        let Some(in_flight) = self.in_flight.remove(peer_id) else {
            return VerificationResult::NotPending;
        };

        let gossiped_overlay = in_flight.gossiped_overlay;
        self.in_flight_overlays.remove(&gossiped_overlay);

        let Some(pending) = self.pending.remove(&gossiped_overlay) else {
            return VerificationResult::NotPending;
        };

        self.decrement_gossiper_count(&pending.gossiper);

        debug!(
            overlay = %gossiped_overlay,
            gossiper = %pending.gossiper,
            "Gossip verification failed: peer unreachable"
        );

        VerificationResult::Unreachable {
            gossiper: pending.gossiper,
        }
    }

    /// Get statistics for metrics reporting.
    pub(crate) fn stats(&self) -> GossipVerifierStats {
        // Memory estimation:
        // - PendingVerification: ~256 bytes (SwarmPeer ~200, gossiper 32, instant 16, PeerId 38)
        // - InFlightVerification: ~64 bytes (overlay 32, instant 16, padding)
        // - Per-gossiper entry: ~40 bytes (overlay 32, count 8)
        const PENDING_ENTRY_SIZE: usize = 256;
        const IN_FLIGHT_ENTRY_SIZE: usize = 64;
        const GOSSIPER_ENTRY_SIZE: usize = 40;

        let estimated_memory_bytes = self.pending.len() * PENDING_ENTRY_SIZE
            + self.in_flight.len() * IN_FLIGHT_ENTRY_SIZE
            + self.per_gossiper_count.len() * GOSSIPER_ENTRY_SIZE;

        GossipVerifierStats {
            pending_count: self.pending.len(),
            in_flight_count: self.in_flight.len(),
            tracked_gossipers: self.per_gossiper_count.len(),
            estimated_memory_bytes,
        }
    }

    #[cfg(test)]
    fn pending_count(&self) -> usize {
        self.pending.len()
    }

    fn remove_pending(&mut self, overlay: &OverlayAddress) {
        if let Some(pending) = self.pending.remove(overlay) {
            self.decrement_gossiper_count(&pending.gossiper);
        }
    }

    fn decrement_gossiper_count(&mut self, gossiper: &OverlayAddress) {
        if let Some(count) = self.per_gossiper_count.get_mut(gossiper) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.per_gossiper_count.remove(gossiper);
            }
        }
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

    /// Generate a deterministic PeerId from an index.
    fn test_peer_id(index: u8) -> PeerId {
        let mut bytes = [0u8; 32];
        bytes[0] = index;
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes).unwrap();
        let keypair = libp2p::identity::ed25519::Keypair::from(key);
        PeerId::from_public_key(&libp2p::identity::PublicKey::from(keypair.public()))
    }

    fn mock_swarm_peer_with_peer_id(overlay_bytes: [u8; 32], addr: &str, peer_id: PeerId) -> SwarmPeer {
        let addr_with_p2p = format!("{}/p2p/{}", addr, peer_id);
        let multiaddr: Multiaddr = addr_with_p2p.parse().unwrap();
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
        let multiaddr: Multiaddr = addr_with_p2p.parse().unwrap();
        SwarmPeer::from_validated(
            vec![multiaddr],
            Signature::new(U256::from(sig_r), U256::from(sig_s), false),
            B256::from(overlay_bytes),
            B256::ZERO,
            Address::ZERO,
        )
    }

    fn mock_swarm_peer_no_p2p(overlay_bytes: [u8; 32], addr: &str) -> SwarmPeer {
        let multiaddr: Multiaddr = addr.parse().unwrap();
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
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        // Multiaddr without /p2p/ component
        let peer = mock_swarm_peer_no_p2p([2u8; 32], "/ip4/1.2.3.4/tcp/1634");
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        let result = verifier.check_gossip(peer, gossiper, None);
        assert!(matches!(
            result,
            GossipCheckResult::Rejected(GossipRejectionReason::NoPeerId)
        ));
    }

    #[test]
    fn test_rejects_self_overlay() {
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        let peer_id = test_peer_id(1);
        let peer = mock_swarm_peer_with_peer_id([1u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([2u8; 32]));

        let result = verifier.check_gossip(peer, gossiper, None);
        assert!(matches!(
            result,
            GossipCheckResult::Rejected(GossipRejectionReason::SelfOverlay)
        ));
    }

    #[test]
    fn test_already_known_with_matching_signature() {
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        let peer_id = test_peer_id(2);
        let peer = mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        let existing = peer.clone();
        let result = verifier.check_gossip(peer, gossiper, Some(&existing));
        assert!(matches!(result, GossipCheckResult::AlreadyKnown));
    }

    #[test]
    fn test_needs_verification_for_new_peer() {
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        let peer_id = test_peer_id(2);
        let peer = mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        let result = verifier.check_gossip(peer, gossiper, None);
        assert!(matches!(result, GossipCheckResult::NeedsVerification(_)));
        assert_eq!(verifier.pending_count(), 1);
    }

    #[test]
    fn test_rate_limit_per_gossiper() {
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        // Add MAX_PENDING_PER_GOSSIPER peers
        for i in 0..MAX_PENDING_PER_GOSSIPER {
            let peer_id = test_peer_id((i + 10) as u8);
            let mut overlay_bytes = [0u8; 32];
            overlay_bytes[0] = (i + 10) as u8;
            let peer = mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);
            let result = verifier.check_gossip(peer, gossiper, None);
            assert!(matches!(result, GossipCheckResult::NeedsVerification(_)));
        }

        // Next should be rate limited
        let peer_id = test_peer_id(100);
        let peer = mock_swarm_peer_with_peer_id([100u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let result = verifier.check_gossip(peer, gossiper, None);
        assert!(matches!(
            result,
            GossipCheckResult::Rejected(GossipRejectionReason::GossiperRateLimited)
        ));
    }

    #[test]
    fn test_verification_success() {
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        let peer_id = test_peer_id(2);
        let peer = mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        verifier.check_gossip(peer.clone(), gossiper, None);

        // Simulate dial and handshake
        let dial = verifier.next_verification_dial().unwrap();
        let result = verifier.on_verification_handshake(dial.peer_id, peer);

        assert!(matches!(result, VerificationResult::Verified { .. }));
        assert_eq!(verifier.pending_count(), 0);
    }

    #[test]
    fn test_verification_identity_updated() {
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        let peer_id = test_peer_id(2);
        // Gossiped peer with signature (1, 2)
        let gossiped_peer = mock_swarm_peer_with_signature([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id, 1, 2);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        verifier.check_gossip(gossiped_peer, gossiper, None);
        let dial = verifier.next_verification_dial().unwrap();

        // Verified peer has same overlay but different signature (3, 4)
        let verified_peer = mock_swarm_peer_with_signature([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id, 3, 4);
        let result = verifier.on_verification_handshake(dial.peer_id, verified_peer);

        assert!(matches!(result, VerificationResult::IdentityUpdated { .. }));
    }

    #[test]
    fn test_verification_different_peer_at_address() {
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        let peer_id = test_peer_id(2);
        let gossiped_peer = mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));
        let gossiped_overlay = OverlayAddress::from(*gossiped_peer.overlay());

        verifier.check_gossip(gossiped_peer, gossiper, None);
        let dial = verifier.next_verification_dial().unwrap();

        // Handshake returns completely different overlay
        let verified_peer = mock_swarm_peer_with_peer_id([99u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let result = verifier.on_verification_handshake(dial.peer_id, verified_peer);

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
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        let peer_id = test_peer_id(2);
        let peer = mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        verifier.check_gossip(peer, gossiper, None);
        assert!(!verifier.is_in_flight(&peer_id));

        let dial = verifier.next_verification_dial().unwrap();
        assert!(verifier.is_in_flight(&dial.peer_id));
    }

    #[test]
    fn test_verification_dial_failed() {
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        let peer_id = test_peer_id(2);
        let peer = mock_swarm_peer_with_peer_id([2u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([3u8; 32]));

        verifier.check_gossip(peer, gossiper, None);
        let dial = verifier.next_verification_dial().unwrap();

        let result = verifier.on_verification_dial_failed(&dial.peer_id);
        assert!(matches!(result, VerificationResult::Unreachable { .. }));
        assert_eq!(verifier.pending_count(), 0);
    }

    #[test]
    fn test_total_pending_limit() {
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        // Use many gossipers to avoid per-gossiper rate limit
        for i in 0..MAX_TOTAL_PENDING {
            let peer_id = test_peer_id((i + 10) as u8);
            let mut overlay_bytes = [0u8; 32];
            overlay_bytes[0] = (i + 10) as u8;
            overlay_bytes[1] = (i / 256) as u8;
            let peer = mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);

            // Each peer from a different gossiper
            let mut gossiper_bytes = [0u8; 32];
            gossiper_bytes[0] = (i + 100) as u8;
            gossiper_bytes[1] = (i / 256) as u8;
            let gossiper = OverlayAddress::from(B256::from(gossiper_bytes));

            let result = verifier.check_gossip(peer, gossiper, None);
            assert!(
                matches!(result, GossipCheckResult::NeedsVerification(_)),
                "Expected NeedsVerification for peer {}, got {:?}", i, result
            );
        }

        assert_eq!(verifier.pending_count(), MAX_TOTAL_PENDING);

        // Next should be rejected with QueueFull
        let peer_id = test_peer_id(250);
        let peer = mock_swarm_peer_with_peer_id([250u8; 32], "/ip4/1.2.3.4/tcp/1634", peer_id);
        let gossiper = OverlayAddress::from(B256::from([251u8; 32]));
        let result = verifier.check_gossip(peer, gossiper, None);
        assert!(matches!(
            result,
            GossipCheckResult::Rejected(GossipRejectionReason::QueueFull)
        ));
    }

    #[test]
    fn test_multiple_gossipers_tracked() {
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        // Track multiple gossipers to verify LRU doesn't break rate limiting
        for g in 0..10 {
            let gossiper = OverlayAddress::from(B256::from([(g + 100) as u8; 32]));

            for i in 0..5 {
                let peer_id = test_peer_id((g * 10 + i + 20) as u8);
                let mut overlay_bytes = [0u8; 32];
                overlay_bytes[0] = (g * 10 + i + 20) as u8;
                let peer = mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);

                let result = verifier.check_gossip(peer, gossiper, None);
                assert!(matches!(result, GossipCheckResult::NeedsVerification(_)));
            }
        }

        // 10 gossipers * 5 peers each = 50 pending
        assert_eq!(verifier.pending_count(), 50);
    }

    #[test]
    fn test_concurrent_verifications_limit() {
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        // Add more peers than MAX_CONCURRENT_VERIFICATIONS
        for i in 0..MAX_CONCURRENT_VERIFICATIONS + 5 {
            let peer_id = test_peer_id((i + 10) as u8);
            let mut overlay_bytes = [0u8; 32];
            overlay_bytes[0] = (i + 10) as u8;
            let peer = mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);
            let gossiper = OverlayAddress::from(B256::from([3u8; 32]));
            verifier.check_gossip(peer, gossiper, None);
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
        let local = OverlayAddress::from(B256::from([1u8; 32]));
        let mut verifier = GossipVerifier::new(local);

        // Add 10 peers
        for i in 0..10 {
            let peer_id = test_peer_id((i + 10) as u8);
            let mut overlay_bytes = [0u8; 32];
            overlay_bytes[0] = (i + 10) as u8;
            let peer = mock_swarm_peer_with_peer_id(overlay_bytes, "/ip4/1.2.3.4/tcp/1634", peer_id);
            let gossiper = OverlayAddress::from(B256::from([3u8; 32]));
            verifier.check_gossip(peer, gossiper, None);
        }

        // Get batch of 5
        let batch = verifier.next_verification_batch(5);
        assert_eq!(batch.len(), 5);

        // All should be in-flight now
        for pending in &batch {
            assert!(verifier.is_in_flight(&pending.peer_id));
        }

        // Get another batch (should get remaining 5)
        let batch2 = verifier.next_verification_batch(10);
        assert_eq!(batch2.len(), 5);

        // No more available
        let batch3 = verifier.next_verification_batch(10);
        assert!(batch3.is_empty());
    }
}
