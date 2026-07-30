//! Cold-start convergence: an honest gossiped population saturating bins 0
//! and 1 with a low-watermark anchor in bin 2 climbs the node to depth 2.

mod common;

use std::time::Duration;

use common::{await_handle, family, gossip, launch_node, place, run_until, spec};
use vertex_swarm_api::DEFAULT_SATURATION_PEERS;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_sim::{Invariants, PeerScript, Scenario, SimWorld};
use vertex_swarm_topology::KademliaConfig;

const SEED: u64 = 41;
const STORER: SwarmNodeType = SwarmNodeType::Storer;

#[test]
fn converges_from_cold_start() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(48 * 3600))
        .tick(Duration::from_millis(50))
        .tokio_io()
        .build();
    let probe = launch_node(&mut world, KademliaConfig::default());

    let sat = usize::from(DEFAULT_SATURATION_PEERS);
    let mut scenario = Scenario::new(&world, spec());
    let mut names = Vec::new();
    for (bin, count) in [(0u8, sat), (1, sat), (2, 3)] {
        for name in family(&format!("bin{bin}"), count) {
            scenario.add_peer(
                &mut world,
                &name,
                STORER,
                PeerScript::Honest,
                place(SEED, bin),
            );
            names.push(name);
        }
    }

    let handle = await_handle(&mut world, &probe);
    gossip(&handle, &mut scenario, &names);

    let converged = run_until(
        &mut world,
        Duration::from_secs(600),
        Duration::from_secs(2),
        || handle.routing_stats().depth == 2,
    );
    assert!(
        converged,
        "honest population never converged to the anchored depth (seed={SEED}): {:?}",
        handle.routing_stats()
    );

    // The climb is stable: the depth holds and the counters stay coherent.
    world
        .run_for(Duration::from_secs(60))
        .expect("world advances");
    let stats = handle.routing_stats();
    assert_eq!(stats.depth, 2, "the anchored depth holds once reached");
    Invariants::new()
        .phase_counters_consistent()
        .saturation_floor(sat)
        .assert(&stats, SEED);
}
