//! Scenario suite over the deterministic sim: convergence, churn stability,
//! partition heal, starvation truth, saturation-floor safety, sub-prefix-slot
//! dilution of a monoculture, and a characterization test pinning the current
//! direction-blind fill behaviour.
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
async fn inbound_flood_suppresses_outbound() {
    // Characterization: the fill target is direction-blind. A bin flooded to its
    // ceiling by inbound connections satisfies the target, so the evaluation
    // loop queues no outbound dial for it even though known, dialable outbound
    // peers exist in the same bin. A later direction-aware fill flips this.
    // `nominal` is raised above the flood size so the freshly-known inbound
    // peers do not inflate the estimated depth: the bin stays a finite-target
    // bootstrap bin rather than being promoted to an unlimited neighborhood bin.
    let config = KademliaConfig::default()
        .with_bootstrap_target(4)
        .with_saturation(4)
        .with_oversaturation_peers(4)
        .with_inbound_headroom(0)
        .with_nominal(10);
    let mut world = SimWorld::new(config, 16, 13);
    let bin = 5u8;

    // Four known, dialable outbound-capable peers in the bin. If the bin had
    // room the evaluator would dial these.
    for idx in 0..4u8 {
        let overlay = super::overlay_in_bin(world.base(), bin, idx);
        world.add_scripted(overlay, STORER, PeerScript::Honest);
    }

    // Flood the bin's inbound ceiling with a disjoint set of overlays.
    let mut flooded = 0;
    for idx in 100..108u8 {
        let overlay = super::overlay_in_bin(world.base(), bin, idx);
        if world.flood_inbound(overlay, STORER) {
            flooded += 1;
        }
    }
    assert_eq!(flooded, 4, "inbound is accepted up to the ceiling");

    for _ in 0..10 {
        world.tick();
    }

    assert_eq!(
        world.connected_in_bin(bin),
        4,
        "the fill target is satisfied by inbound alone"
    );
    let (dialing, handshaking, active) = world.bin_phase_counts(bin);
    assert_eq!(
        dialing + handshaking,
        0,
        "no outbound dial is ever launched for the flooded bin"
    );
    assert_eq!(active, 4, "the four inbound peers are the only actives");

    world.evaluate();
    assert!(
        !world.has_queued_candidates(),
        "the direction-blind fill leaves the evaluator quiet"
    );
}
