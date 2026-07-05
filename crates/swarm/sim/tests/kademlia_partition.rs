//! Partition heal: crashing everything deeper than bin 0 collapses the
//! published depth at once (a multi-bin deficit is never held), the collapse
//! holds while every re-dial of the known supply fails, and depth re-climbs
//! only from freshly gossiped supply.

mod common;

use std::time::Duration;

use common::{await_handle, family, gossip, launch_node, place, run_until, spec};
use vertex_swarm_api::DEFAULT_SATURATION_PEERS;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_sim::{Invariants, PeerScript, Scenario, SimWorld};
use vertex_swarm_topology::KademliaConfig;

const SEED: u64 = 47;
const STORER: SwarmNodeType = SwarmNodeType::Storer;

#[test]
fn partition_collapses_then_heals_by_rediscovery() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(48 * 3600))
        .tick(Duration::from_millis(50))
        .tokio_io()
        .build();
    let probe = launch_node(&mut world, KademliaConfig::default());
    // Derive from the built world seed so a replay override reproduces exactly.
    let seed = world.seed();

    let sat = usize::from(DEFAULT_SATURATION_PEERS);
    let mut scenario = Scenario::new(&world, spec());
    let mut names = Vec::new();
    let mut severed = Vec::new();
    for (bin, count) in [(0u8, sat), (1, sat), (2, 3)] {
        for name in family(&format!("bin{bin}"), count) {
            scenario.add_peer(
                &mut world,
                &name,
                STORER,
                PeerScript::Honest,
                place(seed, bin),
            );
            if bin > 0 {
                severed.push(name.clone());
            }
            names.push(name);
        }
    }
    // The far side of the heal: registered and listening from the start, but
    // unknown to the node until gossiped, so the heal is pure re-discovery.
    let mut heal = Vec::new();
    for (bin, count) in [(1u8, sat), (2, 3)] {
        for name in family(&format!("heal{bin}"), count) {
            scenario.add_peer(
                &mut world,
                &name,
                STORER,
                PeerScript::Honest,
                place(seed, bin),
            );
            heal.push(name);
        }
    }

    let (handle, _marker) = await_handle(&mut world, &probe);
    gossip(&handle, &mut scenario, &names);
    let converged = run_until(
        &mut world,
        Duration::from_secs(600),
        Duration::from_secs(2),
        || handle.routing_stats().depth == 2,
    );
    assert!(converged, "fixture never converged (seed={seed})");

    // Partition: the far side of the cut is gone, not merely disconnected.
    for name in &severed {
        world.crash(name);
    }
    // Well inside the depth-lowering window: a multi-bin deficit must not
    // ride the marginal-deficit hold.
    let collapsed = run_until(
        &mut world,
        Duration::from_secs(15),
        Duration::from_millis(250),
        || handle.routing_stats().depth <= 1,
    );
    assert!(
        collapsed,
        "a multi-bin loss must lower the published depth at once (seed={seed}): {:?}",
        handle.routing_stats()
    );

    // The severed peers stay known, so the evaluator re-dials them whenever
    // a backoff window expires; every attempt fails against the crashed
    // hosts and re-arms a doubled window. With no reachable supply the
    // collapse holds.
    let deadline = world.elapsed() + Duration::from_secs(120);
    while world.elapsed() < deadline {
        world
            .run_for(Duration::from_secs(2))
            .expect("world advances");
        assert!(
            handle.routing_stats().depth <= 1,
            "depth must not heal without reachable supply (seed={seed})"
        );
    }

    // Heal: gossip a fresh population on the far side. Depth re-climbs from
    // the new supply alone (re-dials of the severed supply keep failing).
    gossip(&handle, &mut scenario, &heal);
    let healed = run_until(
        &mut world,
        Duration::from_secs(600),
        Duration::from_secs(2),
        || handle.routing_stats().depth == 2,
    );
    assert!(
        healed,
        "depth never re-climbed from the new supply (seed={seed}): {:?}",
        handle.routing_stats()
    );
    Invariants::new()
        .phase_counters_consistent()
        .assert(&handle.routing_stats(), seed);
}
