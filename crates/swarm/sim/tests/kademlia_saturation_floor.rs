//! Saturation floor: a hostile total_target far below saturation times the
//! bin count would taper below-depth bins under saturation, but the
//! allocation floor overrides it, so depth still climbs, holds, and refills.

mod common;

use std::time::Duration;

use common::{
    await_handle, connected_in_bin, family, gossip, launch_node, node_overlay, run_until, spec,
};
use vertex_swarm_api::DEFAULT_SATURATION_PEERS;
use vertex_swarm_primitives::{Bin, SwarmNodeType};
use vertex_swarm_sim::{Invariants, PeerScript, Placement, Scenario, SimWorld};
use vertex_swarm_topology::KademliaConfig;

const SEED: u64 = 9;
const STORER: SwarmNodeType = SwarmNodeType::Storer;

#[test]
fn saturation_floor_survives_hostile_taper() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(48 * 3600))
        .tick(Duration::from_millis(50))
        .tokio_io()
        .build();
    // total_target 4 gives a raw taper of one to two peers per below-depth
    // bin; only the saturation floor lets the frontier form.
    let probe = launch_node(&mut world, KademliaConfig::default().with_total_target(4));
    // Derive from the built world seed so a replay override reproduces exactly.
    let seed = world.seed();

    // Every balanced-bin peer shares sub-prefix slot 0, so a refill can only
    // come through the count-deficit path the floor governs, never through
    // the empty-slot diversity pass.
    let sat = usize::from(DEFAULT_SATURATION_PEERS);
    let anchor = node_overlay(seed);
    let slot0 =
        |bin: u8| Some(Placement::new(anchor, Bin::new(bin).unwrap_or(Bin::MAX)).in_slot(0));
    let mut scenario = Scenario::new(&world, spec());
    let mut names = Vec::new();
    for (bin, count) in [(0u8, sat), (1, sat)] {
        for name in family(&format!("bin{bin}"), count) {
            scenario.add_peer(&mut world, &name, STORER, PeerScript::Honest, slot0(bin));
            names.push(name);
        }
    }
    for name in family("bin2", 3) {
        scenario.add_peer(
            &mut world,
            &name,
            STORER,
            PeerScript::Honest,
            Some(Placement::new(anchor, Bin::new(2).unwrap_or(Bin::MAX))),
        );
        names.push(name);
    }

    let (handle, _marker) = await_handle(&mut world, &probe);
    gossip(&handle, &mut scenario, &names);
    let converged = run_until(
        &mut world,
        Duration::from_secs(600),
        Duration::from_secs(2),
        || handle.routing_stats().depth == 2,
    );
    assert!(
        converged,
        "the saturation floor must let depth climb despite the hostile taper (seed={seed}): {:?}",
        handle.routing_stats()
    );

    // The floor is visible in the published targets and the held populations.
    let floor = Invariants::new()
        .phase_counters_consistent()
        .saturation_floor(sat);
    let stats = handle.routing_stats();
    floor.assert(&stats, seed);
    for bin in 0..2u8 {
        assert!(
            connected_in_bin(&stats, bin) >= sat,
            "below-depth bin {bin} fell under saturation (seed={seed}): {stats:?}"
        );
    }

    // A churned frontier peer is refilled toward the floor, not toward the
    // raw taper target the hostile configuration asked for. If the bounce
    // reads as an early disconnect, the armed backoff expires under virtual
    // advance inside the refill window.
    world.bounce("bin0-0");
    let refilled = run_until(
        &mut world,
        Duration::from_secs(120),
        Duration::from_millis(500),
        || connected_in_bin(&handle.routing_stats(), 0) >= sat,
    );
    assert!(
        refilled,
        "the floored bin was never refilled after churn (seed={seed}): {:?}",
        handle.routing_stats()
    );
    assert_eq!(
        handle.routing_stats().depth,
        2,
        "depth holds through the churn"
    );
    floor.assert(&handle.routing_stats(), seed);
}
