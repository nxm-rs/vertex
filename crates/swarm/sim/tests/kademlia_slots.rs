//! Sub-prefix slot dilution: a balanced bin count-satisfied by a one-slot
//! sybil monoculture still pulls freshly gossiped diverse supply through the
//! empty-slot pass, retains it against trimming, and never dials
//! duplicate-slot supply past the count target.

mod common;

use std::time::Duration;

use common::{
    await_handle, family, gossip, launch_node, node_overlay, run_until, spec, trace_count,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_primitives::{Bin, SwarmNodeType};
use vertex_swarm_sim::{PeerScript, Placement, Scenario, SimWorld};
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};

const SEED: u64 = 11;
const STORER: SwarmNodeType = SwarmNodeType::Storer;
const SYBIL_SLOT: u8 = 5;

fn bin0_slots(handle: &TopologyHandle<Identity>) -> Option<usize> {
    handle
        .readiness()
        .bins
        .iter()
        .find(|bin| bin.bin.get() == 0)
        .and_then(|bin| bin.slots_filled)
}

#[test]
fn empty_slots_dilute_a_subprefix_monoculture() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(48 * 3600))
        .tick(Duration::from_millis(50))
        .tokio_io()
        .build();
    // total_target 8 pins bin 0's dial target at exactly saturation, so the
    // sybil family alone satisfies the count and only slot awareness can
    // justify further dials.
    let probe = launch_node(&mut world, KademliaConfig::default().with_total_target(8));
    // Derive from the built world seed so a replay override reproduces exactly.
    let seed = world.seed();

    let anchor = node_overlay(seed);
    let place =
        |slot: u8| Some(Placement::new(anchor, Bin::new(0).unwrap_or(Bin::MAX)).in_slot(slot));
    let mut scenario = Scenario::new(&world, spec());

    // Phase-1 supply: eight sybils clustered in one sub-prefix slot of bin 0,
    // and a bin-1 anchor holding the depth at 1.
    let mut first = Vec::new();
    for name in family("sybil", 8) {
        scenario.add_peer(
            &mut world,
            &name,
            STORER,
            PeerScript::Sybil,
            place(SYBIL_SLOT),
        );
        first.push(name);
    }
    for name in family("anchor", 3) {
        scenario.add_peer(
            &mut world,
            &name,
            STORER,
            PeerScript::Honest,
            Some(Placement::new(anchor, Bin::new(1).unwrap_or(Bin::MAX))),
        );
        first.push(name);
    }
    // Phase-2 supply: four honest peers in four distinct empty slots, plus
    // one more clustered in the sybil slot that must stay undialled.
    let mut diverse = Vec::new();
    for (idx, slot) in (1u8..=4).enumerate() {
        let name = format!("fresh-{idx}");
        scenario.add_peer(&mut world, &name, STORER, PeerScript::Honest, place(slot));
        diverse.push(name);
    }
    scenario.add_peer(
        &mut world,
        "dup",
        STORER,
        PeerScript::Honest,
        place(SYBIL_SLOT),
    );
    diverse.push("dup".to_owned());

    let handle = await_handle(&mut world, &probe);
    gossip(&handle, &mut scenario, &first);
    let monoculture = run_until(
        &mut world,
        Duration::from_secs(600),
        Duration::from_secs(2),
        || {
            let stats = handle.routing_stats();
            stats.depth == 1 && common::connected_in_bin(&stats, 0) == 8
        },
    );
    assert!(
        monoculture,
        "the sybil supply never met the count target (seed={seed}): {:?}",
        handle.routing_stats()
    );
    assert_eq!(
        bin0_slots(&handle),
        Some(1),
        "the sole supply shares one slot: sub-prefix monoculture (seed={seed})"
    );

    // Diverse supply arrives. The empty slots pull it in even though the
    // count target is already met; the duplicate-slot peer is left alone.
    gossip(&handle, &mut scenario, &diverse);
    let diluted = run_until(
        &mut world,
        Duration::from_secs(300),
        Duration::from_secs(2),
        || common::connected_in_bin(&handle.routing_stats(), 0) == 12,
    );
    assert!(
        diluted,
        "empty slots never pulled the diverse supply (seed={seed}): {:?}",
        handle.routing_stats()
    );
    assert_eq!(
        bin0_slots(&handle),
        Some(5),
        "the monoculture is diluted: the sybil slot plus four fresh slots"
    );

    // The filled set is stable: no trim oscillation churns the diversity
    // out, and the duplicate-slot peer is never dialled past the target.
    world
        .run_for(Duration::from_secs(300))
        .expect("world advances");
    assert_eq!(
        common::connected_in_bin(&handle.routing_stats(), 0),
        12,
        "the diverse set holds without oscillation (seed={seed})"
    );
    assert_eq!(bin0_slots(&handle), Some(5), "slot diversity is retained");
    assert_eq!(
        trace_count(&world, "dup", "incoming"),
        0,
        "duplicate-slot supply is never dialled past the count target"
    );
}
