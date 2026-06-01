//! Integration test for Unit 8: typed saturation decisions.
//!
//! Drives the [`BinPolicy`] surface end-to-end with a fake routing
//! bin: N+1 peers in a single bin, then asserts that
//!
//! * [`StandardPolicy`] returns
//!   [`SaturationDecision::Reject(RejectionReason::BinFull)`].
//! * [`BootnodePolicy`] returns
//!   [`SaturationDecision::AcceptEvict(...)`] pointing at the worst
//!   peer (lowest reachability, then lowest score, then oldest
//!   `last_seen`).
//!
//! This is the e2e gate for the saturation typing PR: future units
//! (handshake admission control, bootnode wiring) consume the same
//! decision surface, so this test pins the contract independent of
//! anything inside the routing layer.

use vertex_swarm_test_utils::{make_overlay, test_peer_id};

use vertex_swarm_topology::saturation::{
    BinPolicy, BootnodePolicy, EvictReason, PeerReachability, PeerStats, RejectionReason,
    SaturationContext, SaturationDecision, StandardPolicy, UnknownReachability,
};

// The saturation module's `DepthAwareLimits` is a crate-private type
// but `SaturationContext::new` accepts a borrow. We need an instance
// to construct contexts — re-use one of the topology helpers. The
// kademlia module exposes `KademliaConfig`, which holds the limits we
// need; we route through a config to obtain a real `DepthAwareLimits`.
use vertex_swarm_topology::KademliaConfig;

/// Bin 0 ceiling at depth 8 with the default limits.
///
/// `DepthAwareLimits::new(160, 3)` -> target(0, 8) = max(160/36, 3) = 4,
/// ceiling = target + inbound_headroom(4) = 8. We populate the bin with
/// 9 peers and effective_count = 9 so the policies see over-saturation.
const N_PEERS_OVER_CEILING: usize = 9;

fn peer(byte: u8, score: f64, last_seen: u64) -> PeerStats {
    PeerStats {
        peer_id: test_peer_id(byte),
        overlay: make_overlay(byte),
        score,
        last_seen,
    }
}

#[test]
fn standard_policy_rejects_oversaturated_bin() {
    let cfg = KademliaConfig::default();
    let limits = cfg.depth_aware_limits();
    let reach = UnknownReachability;

    let peers: Vec<PeerStats> = (0..N_PEERS_OVER_CEILING as u8)
        .map(|i| peer(0x80 + i, 0.0, 100))
        .collect();

    let ctx = SaturationContext::new(0, 8, peers.len(), limits, &peers, &reach);
    assert!(ctx.is_oversaturated(), "test setup should over-saturate");
    assert_eq!(
        StandardPolicy.decide(&ctx),
        SaturationDecision::Reject(RejectionReason::BinFull),
        "non-bootnode policy must reject when bin is over-saturated"
    );
}

#[test]
fn bootnode_policy_evicts_worst_peer_in_oversaturated_bin() {
    let cfg = KademliaConfig::default();
    let limits = cfg.depth_aware_limits();
    let reach = UnknownReachability;

    // Build N+1 peers with distinct scores; the lowest-score one is the
    // expected eviction target under the `(reach, score, last_seen,
    // peer_id)` ordering.
    let worst = peer(0x80, -90.0, 100);
    let middle: Vec<PeerStats> = (1..N_PEERS_OVER_CEILING as u8)
        .map(|i| peer(0x80 + i, (i as f64) * 10.0, 100))
        .collect();
    let mut peers = vec![worst];
    peers.extend(middle);

    let ctx = SaturationContext::new(0, 8, peers.len(), limits, &peers, &reach);
    match BootnodePolicy.decide(&ctx) {
        SaturationDecision::AcceptEvict(c) => {
            assert_eq!(c.overlay, worst.overlay, "evictee must be lowest-scored");
            assert_eq!(c.peer_id, worst.peer_id);
            assert_eq!(c.reason, EvictReason::LowestReachability);
        }
        other => panic!("expected AcceptEvict, got {other:?}"),
    }
}

#[test]
fn bootnode_policy_reachability_dominates_score() {
    // Verify the dominant axis: even with the best score, an Unknown
    // peer is evicted before a Public peer with a worse score. To
    // isolate the reachability axis, every peer in the bin is marked
    // `Public` *except* `unknown_peer`. That peer has the highest score
    // among them; ordering by score alone would keep it. Only the
    // reachability axis can flip the choice — which is the contract
    // bootnode mode depends on.
    use libp2p::PeerId;
    use std::collections::HashMap;
    use vertex_swarm_topology::saturation::ReachabilityProvider;

    struct StubReach(HashMap<PeerId, PeerReachability>);
    impl ReachabilityProvider for StubReach {
        fn reachability(&self, peer: &PeerId) -> PeerReachability {
            self.0.get(peer).copied().unwrap_or_default()
        }
    }

    let cfg = KademliaConfig::default();
    let limits = cfg.depth_aware_limits();

    // The one Unknown peer has the best score; everyone else is Public
    // but with worse scores. Lowest score among Public peers is the
    // would-be victim if score dominated — but Unknown < Public must
    // override that.
    let unknown_peer = peer(0x80, 90.0, 100);
    let public_peers: Vec<PeerStats> = (1..N_PEERS_OVER_CEILING as u8)
        .map(|i| peer(0x80 + i, -((i as f64) * 10.0), 100))
        .collect();
    let mut peers = vec![unknown_peer];
    peers.extend(public_peers.iter().copied());

    let mut map = HashMap::new();
    for p in &public_peers {
        map.insert(p.peer_id, PeerReachability::Public);
    }
    let reach = StubReach(map);

    let ctx = SaturationContext::new(0, 8, peers.len(), limits, &peers, &reach);
    match BootnodePolicy.decide(&ctx) {
        SaturationDecision::AcceptEvict(c) => {
            assert_eq!(
                c.overlay, unknown_peer.overlay,
                "Unknown peer must be evicted before any Public peer, \
                 regardless of score"
            );
        }
        other => panic!("expected AcceptEvict, got {other:?}"),
    }
}

#[test]
fn standard_and_bootnode_agree_when_under_ceiling() {
    let cfg = KademliaConfig::default();
    let limits = cfg.depth_aware_limits();
    let reach = UnknownReachability;
    let peers: Vec<PeerStats> = (0..3).map(|i| peer(0x80 + i, 0.0, 100)).collect();
    let ctx = SaturationContext::new(0, 8, 3, limits, &peers, &reach);
    assert_eq!(StandardPolicy.decide(&ctx), SaturationDecision::Accept);
    assert_eq!(BootnodePolicy.decide(&ctx), SaturationDecision::Accept);
}
