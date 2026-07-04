//! Scenario suite over the deterministic sim: convergence, churn stability,
//! partition heal, starvation truth, saturation-floor safety, sub-prefix-slot
//! dilution of a monoculture, the minimum-outbound quota holding a bin's
//! self-dialed share under an inbound eclipse flood, and the readiness clock
//! surviving a boundary-peer flap.
//!
//! Time is paused so the tokio-driven depth-hysteresis clock is deterministic;
//! the phase window and dial backoff run on wall clocks, so nothing here asserts
//! their expiry, only exclusion and holding.

use super::{PeerScript, SimWorld};
use crate::kademlia::KademliaConfig;
use vertex_swarm_primitives::SwarmNodeType;

const STORER: SwarmNodeType = SwarmNodeType::Storer;

#[tokio::test(start_paused = true)]
async fn converges_from_cold_start() {
    // Honest uniform supply: bins 0 and 1 saturated, bin 2 at the low watermark
    // anchors the neighborhood at depth 2.
    let mut world = SimWorld::new(KademliaConfig::default(), 16, 1);
    let sat = world.saturation();
    world.populate_bin(0, sat, STORER, PeerScript::Honest);
    world.populate_bin(1, sat, STORER, PeerScript::Honest);
    world.populate_bin(2, 3, STORER, PeerScript::Honest);

    for _ in 0..50 {
        world.tick();
    }

    assert_eq!(
        world.depth().get(),
        2,
        "honest population converges to the anchored depth"
    );
}

#[tokio::test(start_paused = true)]
async fn churn_storm_holds_depth() {
    // Well-supplied depth-3 table: each below-depth bin holds a saturated core
    // of honest peers plus four auto-cycling `AcceptThenDrop` peers as headroom,
    // so scripted churn keeps the bin oscillating at or above saturation and the
    // frontier never permanently drops.
    let mut world = SimWorld::new(KademliaConfig::default(), 16, 42);
    let sat = world.saturation();
    for bin in 0..3u8 {
        world.populate_bin(bin, sat, STORER, PeerScript::Honest);
        for idx in 200..204u8 {
            let overlay = super::overlay_in_bin(world.base(), bin, idx);
            world.add_scripted(
                overlay,
                STORER,
                PeerScript::AcceptThenDrop { after_ticks: 3 },
            );
        }
    }
    world.populate_bin(3, 4, STORER, PeerScript::Honest);

    for _ in 0..50 {
        world.tick();
    }
    let converged = world.depth().get();
    assert_eq!(converged, 3, "fixture converges to depth 3");

    // Repeated 30% disconnect bursts: each burst settles back to the converged
    // depth within a few re-dial ticks. A marginal loss is held by the
    // hysteresis; a real loss is raised straight back once the bin refills.
    for _ in 0..20 {
        world.churn_burst(0.30);
        for _ in 0..5 {
            world.tick();
        }
        assert_eq!(
            world.depth().get(),
            converged,
            "published depth returns to the converged value after each burst"
        );
    }

    for _ in 0..50 {
        world.tick();
    }
    assert_eq!(world.depth().get(), converged, "table fully recovers");
}

#[tokio::test(start_paused = true)]
async fn partition_heal() {
    // Converge to depth 3, sever everything beyond bin 0 and make the severed
    // peers unreachable: the far side of the partition is gone, not merely
    // disconnected. Depth collapses at the instant of the cut and stays
    // collapsed while every re-dial of the known supply fails; it re-climbs
    // only once fresh supply is gossiped, so the heal is re-discovery, not a
    // trivial re-dial of the same peers.
    let mut world = SimWorld::new(KademliaConfig::default(), 16, 7);
    let sat = world.saturation();
    world.populate_bin(0, sat, STORER, PeerScript::Honest);
    world.populate_bin(1, sat, STORER, PeerScript::Honest);
    world.populate_bin(2, sat, STORER, PeerScript::Honest);
    world.populate_bin(3, 3, STORER, PeerScript::Honest);

    for _ in 0..50 {
        world.tick();
    }
    assert_eq!(world.depth().get(), 3, "fixture converges to depth 3");

    // Partition: drop every connected peer deeper than bin 0. The multi-bin
    // deficit exceeds the hysteresis tolerance, so the lower publishes at once.
    let severed = world.disconnect_bins_above(0);
    assert!(
        world.depth().get() <= 1,
        "partition collapses the neighborhood depth"
    );
    for overlay in severed {
        world.set_script(overlay, PeerScript::Unreachable);
    }

    // The severed peers stay known, so the evaluator re-dials them; every dial
    // fails and arms backoff. With no reachable supply the collapse holds.
    for _ in 0..10 {
        world.tick();
        assert!(
            world.depth().get() <= 1,
            "depth must not heal without reachable supply"
        );
    }

    // Heal: gossip a fresh population on the far side. Depth re-climbs from
    // the new supply alone (the severed peers hold in dial backoff).
    for bin in 1..3u8 {
        for idx in 100..(100 + sat as u8) {
            let overlay = super::overlay_in_bin(world.base(), bin, idx);
            world.add_scripted(overlay, STORER, PeerScript::Honest);
        }
    }
    for idx in 100..103u8 {
        let overlay = super::overlay_in_bin(world.base(), 3, idx);
        world.add_scripted(overlay, STORER, PeerScript::Honest);
    }
    for _ in 0..50 {
        world.tick();
    }
    assert_eq!(
        world.depth().get(),
        3,
        "depth re-climbs from the new supply"
    );
    world.assert_no_double_count();
}

#[tokio::test(start_paused = true)]
async fn all_backoff_is_starvation_without_probe() {
    // Every known peer is unreachable. After one round each candidate has failed
    // once and entered backoff, so the evaluation loop reaches a fixed point:
    // no candidate is eligible and the queue stays empty. This is the
    // routing-level starvation the behaviour-layer isolation probe exists to
    // break; the sim asserts the fixed point, not the probe.
    let mut world = SimWorld::new(KademliaConfig::default(), 16, 3);
    for bin in 0..4 {
        world.populate_bin(bin, 4, STORER, PeerScript::Unreachable);
    }

    for _ in 0..20 {
        world.tick();
    }

    world.evaluate();
    assert!(
        !world.has_queued_candidates(),
        "an all-backoff table queues nothing"
    );
    assert!(
        world.pop_candidate().is_none(),
        "no candidate is poppable under total backoff"
    );
}

#[tokio::test(start_paused = true)]
async fn saturation_floor_never_violated() {
    // A hostile total_target far below saturation * bins: the linear taper alone
    // would drop below-depth bins under saturation, but the allocation floor
    // overrides it. The per-tick invariant checks the floor directly; here the
    // observable outcome is that depth still climbs and stays anchored.
    let config = KademliaConfig::default().with_total_target(4);
    let mut world = SimWorld::new(config, 16, 9);
    let sat = world.saturation();
    world.populate_bin(0, sat, STORER, PeerScript::Honest);
    world.populate_bin(1, sat, STORER, PeerScript::Honest);
    world.populate_bin(2, sat, STORER, PeerScript::Honest);
    world.populate_bin(3, 3, STORER, PeerScript::Honest);

    for _ in 0..60 {
        world.tick();
    }

    let depth = world.depth().get();
    assert_eq!(
        depth, 3,
        "the saturation floor lets depth climb despite the hostile taper"
    );
    for bin in 0..depth {
        assert!(
            world.connected_in_bin(bin) >= sat,
            "below-depth bin {bin} stays at saturation"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn slot_fill_dilutes_subprefix_monoculture() {
    // A balanced bin is fed first by sybils sharing one sub-prefix slot. While
    // they are the only supply they satisfy the bin's count target and form a
    // monoculture. When diverse honest peers in distinct slots are gossiped
    // afterwards, the empty-slot pass pulls them in up to the retention floor,
    // diluting the monoculture instead of leaving the count-satisfied bin
    // quiet. The bin sits below the published depth (bin 1 anchors depth 1), so
    // the fill runs through the balanced (slot-aware) selection path.
    //
    // The retention floor is max(target, oversaturation) = max(4, 8) = 8, so
    // the eight-peer supply (four sybils plus four honest) is exactly the
    // floor: the fill lands every distinct slot without overshooting it.
    let config = KademliaConfig::default()
        .with_total_target(4)
        .with_bootstrap_target(4)
        .with_saturation(4)
        .with_oversaturation_peers(8);
    let mut world = SimWorld::new(config, 16, 11);
    let target = 4usize;
    let retention_floor = 8usize;
    let bin = 0u8;
    let sybil_slot = 5u8;
    for idx in 0..target as u8 {
        let overlay = super::overlay_in_bin_with_slot(world.base(), bin, sybil_slot, idx);
        world.add_scripted(overlay, STORER, PeerScript::Sybil);
    }
    world.populate_bin(1, 3, STORER, PeerScript::Honest);

    for _ in 0..30 {
        world.tick();
    }

    // Phase 1: while the sybils are the only supply they form a monoculture at
    // the count target, one slot deep.
    assert_eq!(world.depth().get(), 1, "the anchor bin establishes depth 1");
    assert_eq!(
        world.connected_in_bin(bin),
        target,
        "the bin reaches its count target from the sybils alone"
    );
    assert_eq!(
        world.distinct_slots_in_bin(bin),
        1,
        "the sole supply shares one slot: sub-prefix monoculture"
    );

    // Diverse honest peers arrive in four distinct empty slots.
    for (idx, slot) in (1u8..=4).enumerate() {
        let overlay = super::overlay_in_bin_with_slot(world.base(), bin, slot, 100 + idx as u8);
        world.add_scripted(overlay, STORER, PeerScript::Honest);
    }

    // The empty slots pull the diverse supply even though the count target is
    // already met: the count-satisfied bin is no longer absorbing.
    world.evaluate();
    assert!(
        world.has_queued_candidates(),
        "empty slots pull the diverse supply"
    );

    for _ in 0..10 {
        world.tick();
    }

    assert_eq!(
        world.connected_in_bin(bin),
        retention_floor,
        "every distinct slot is filled up to the retention floor"
    );
    assert!(
        world.connected_in_bin(bin) <= retention_floor,
        "empty-slot fills never overshoot the retention floor"
    );
    assert_eq!(
        world.distinct_slots_in_bin(bin),
        5,
        "the monoculture is diluted: the sybil slot plus four honest slots"
    );

    // The filled set is stable: connected sits at the retention floor, so trim
    // reclaims nothing and no dial-trim oscillation churns the diversity out.
    for _ in 0..20 {
        world.tick();
    }
    assert_eq!(
        world.connected_in_bin(bin),
        retention_floor,
        "the diverse set holds at the retention floor without oscillation"
    );
    assert_eq!(
        world.distinct_slots_in_bin(bin),
        5,
        "slot diversity is retained, not trimmed back to the monoculture"
    );
}

#[tokio::test(start_paused = true)]
async fn boundary_flap_holds_readiness_clock() {
    // The neighborhood sits exactly at the saturation threshold and one
    // boundary peer auto-cycles: every drop dips the count a single peer
    // below threshold and the next re-dial restores it. Without the dip
    // damping each cycle would zero the readiness clock and pull-syncing
    // would never gate open. Time is paused, so the dip window never
    // expires: a clock that stays anchored through the cycles is exactly
    // the hysteresis holding.
    let mut world = SimWorld::new(KademliaConfig::default(), 16, 17);
    let sat = world.saturation();
    world.populate_bin(0, sat, STORER, PeerScript::Honest);
    // Bins 1..=3 supply the neighborhood: sat - 1 honest peers plus one
    // cycling boundary peer land the count exactly at the threshold.
    world.populate_bin(1, sat - 4, STORER, PeerScript::Honest);
    world.populate_bin(2, 2, STORER, PeerScript::Honest);
    world.populate_bin(3, 1, STORER, PeerScript::Honest);
    let flapper = super::overlay_in_bin(world.base(), 1, 200);
    world.add_scripted(
        flapper,
        STORER,
        PeerScript::AcceptThenDrop { after_ticks: 2 },
    );

    for _ in 0..30 {
        world.tick();
    }
    assert_eq!(world.depth().get(), 1, "fixture anchors at depth 1");
    assert!(
        world.stable_for().is_some(),
        "the saturated neighborhood carries a readiness clock"
    );

    // Flap cycles: the boundary peer keeps dropping and being re-dialled.
    // The published depth and the readiness clock must both hold.
    for _ in 0..30 {
        world.tick();
        assert_eq!(world.depth().get(), 1, "a one-peer flap never moves depth");
        assert!(
            world.stable_for().is_some(),
            "a one-peer flap never zeroes the readiness clock"
        );
    }

    // A genuine loss is still observed promptly: two honest neighborhood
    // peers going away exceeds the dip tolerance and clears the clock at
    // the second disconnect, with no window.
    let lost = [
        super::overlay_in_bin(world.base(), 2, 0),
        super::overlay_in_bin(world.base(), 2, 1),
    ];
    for overlay in lost {
        world.disconnect(overlay);
    }
    assert!(
        world.stable_for().is_none(),
        "a multi-peer loss clears the readiness clock immediately"
    );
}

#[tokio::test(start_paused = true)]
async fn outbound_quota_holds_under_inbound_flood() {
    // An attacker floods a bin to its count target with inbound sybils, hoping
    // to switch off honest outbound dialling and leave every route into the
    // neighbourhood attacker-created. The minimum-outbound quota resists: the
    // bin still dials outbound up to the quota even though inbound alone met the
    // count target. The target-to-floor band (target 4, oversaturation 8) opens
    // the room; `nominal` is raised above the population so the freshly-known
    // inbound peers do not inflate the estimated depth, keeping the bin a
    // finite-target bootstrap bin rather than an unlimited neighborhood bin.
    let config = KademliaConfig::default()
        .with_bootstrap_target(4)
        .with_saturation(4)
        .with_oversaturation_peers(8)
        .with_inbound_headroom(0)
        .with_nominal(10);
    let mut world = SimWorld::new(config, 16, 13);
    let bin = 5u8;
    // Quota is saturation (4) rounded up over two = 2.
    let quota = 2usize;

    // Four known, dialable outbound-capable peers in the bin.
    for idx in 0..4u8 {
        let overlay = super::overlay_in_bin(world.base(), bin, idx);
        world.add_scripted(overlay, STORER, PeerScript::Honest);
    }

    // Flood to the count target (4), not the raised retention floor (8):
    // flooding to the floor would leave no band for the quota and mask the fix.
    // Every attempt is accepted (ceiling 8 > 4), so the attacker satisfies the
    // fill target with a disjoint inbound set.
    let mut flooded = 0;
    for idx in 100..104u8 {
        let overlay = super::overlay_in_bin(world.base(), bin, idx);
        if world.flood_inbound(overlay, STORER) {
            flooded += 1;
        }
    }
    assert_eq!(
        flooded, 4,
        "attacker satisfies the fill target with inbound"
    );
    assert_eq!(
        world.connected_in_bin(bin),
        4,
        "the count target is met by inbound alone"
    );

    // The direction-aware fill pulls outbound dials into the flooded bin even
    // though the count target is already met.
    world.evaluate();
    assert!(
        world.has_queued_candidates(),
        "outbound quota pulls dials into the flooded bin"
    );

    for _ in 0..10 {
        world.tick();
    }

    assert_eq!(
        world.outbound_in_bin(bin),
        quota,
        "the bin holds its minimum-outbound quota of self-dialed peers"
    );
    assert_eq!(
        world.connected_in_bin(bin),
        4 + quota,
        "four inbound plus the two quota dials, within the retention floor (8)"
    );
    assert_eq!(
        world.connected_in_bin(bin) - world.outbound_in_bin(bin),
        4,
        "the four flooded inbound peers remain connected"
    );

    // No storm: the quota is met, so the evaluator falls quiet and the bin
    // holds steady rather than dialling the band up to the floor.
    world.evaluate();
    assert!(
        !world.has_queued_candidates(),
        "no further dials once the quota is met"
    );
    for _ in 0..10 {
        world.tick();
    }
    assert_eq!(
        world.connected_in_bin(bin),
        4 + quota,
        "the quota-filled bin holds without a dial storm"
    );
    assert_eq!(
        world.outbound_in_bin(bin),
        quota,
        "the outbound share stays at the quota"
    );
}
