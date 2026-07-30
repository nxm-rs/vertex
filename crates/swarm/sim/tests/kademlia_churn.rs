//! Churn storm: repeated seeded bounce bursts across the population; the
//! evaluator re-dials the restarted peers and the converged depth returns
//! after every burst.

mod common;

use std::time::Duration;

use common::{await_handle, family, gossip, launch_node, place, run_until, spec};
use vertex_swarm_api::DEFAULT_SATURATION_PEERS;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_sim::{Fault, FaultSchedule, Invariants, PeerScript, Scenario, SimWorld};
use vertex_swarm_topology::KademliaConfig;

const SEED: u64 = 42;
const STORER: SwarmNodeType = SwarmNodeType::Storer;

#[test]
fn churn_storm_recovers_depth() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(48 * 3600))
        .tick(Duration::from_millis(50))
        .tokio_io()
        .build();
    let probe = launch_node(&mut world, KademliaConfig::default());

    // Two peers of slack per below-depth bin, so a burst that briefly locks a
    // restarted peer into dial backoff cannot starve the refill.
    let sat = usize::from(DEFAULT_SATURATION_PEERS);
    let mut scenario = Scenario::new(&world, spec());
    let mut names = Vec::new();
    for (bin, count) in [(0u8, sat + 2), (1, sat + 2), (2, 4)] {
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
    assert!(converged, "fixture never converged (seed={SEED})");

    let always = Invariants::new()
        .phase_counters_consistent()
        .saturation_floor(sat);

    // Three 30% bounce bursts: each drops connections across the table; the
    // evaluator re-dials the restarted supply and depth returns. Every peer
    // has served the node, so the bursts read as blameless churn and the
    // restarted supply stays immediately re-dialable.
    let overlays: Vec<_> = scenario.peers().iter().map(|p| p.overlay).collect();
    common::mark_overlays_productive(&handle, &overlays);
    let start = world.elapsed();
    FaultSchedule::new()
        .at(
            start + Duration::from_secs(10),
            Fault::Churn { fraction: 0.3 },
        )
        .at(
            start + Duration::from_secs(200),
            Fault::Churn { fraction: 0.3 },
        )
        .at(
            start + Duration::from_secs(400),
            Fault::Churn { fraction: 0.3 },
        )
        .apply(&mut world, &mut scenario, |world, _fault| {
            let recovered = run_until(
                world,
                Duration::from_secs(150),
                Duration::from_secs(2),
                || handle.routing_stats().depth == 2,
            );
            assert!(
                recovered,
                "depth never returned after a burst (seed={SEED}): {:?}",
                handle.routing_stats()
            );
            always.assert(&handle.routing_stats(), SEED);
            // The refilled connections must be blameless for the next burst.
            common::mark_overlays_productive(&handle, &overlays);
        })
        .expect("schedule applies");

    // The table settles fully after the storm.
    world
        .run_for(Duration::from_secs(120))
        .expect("world advances");
    assert_eq!(
        handle.routing_stats().depth,
        2,
        "table recovers after the storm"
    );
    always.assert(&handle.routing_stats(), SEED);
}
