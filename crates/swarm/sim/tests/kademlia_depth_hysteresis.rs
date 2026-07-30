//! Depth hysteresis: a single churning frontier peer never moves the
//! published depth; the one-peer deficit is held while the peer is re-dialled.

mod common;

use std::time::Duration;

use common::{await_handle, connected_in_bin, family, gossip, launch_node, place, run_until, spec};
use vertex_swarm_api::DEFAULT_SATURATION_PEERS;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_sim::{PeerScript, Scenario, SimWorld};
use vertex_swarm_topology::KademliaConfig;

const SEED: u64 = 43;
const STORER: SwarmNodeType = SwarmNodeType::Storer;

#[test]
fn one_peer_flap_never_moves_depth() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(48 * 3600))
        .tick(Duration::from_millis(50))
        .latency(Duration::from_millis(10), Duration::from_millis(50))
        .tokio_io()
        .build();
    let probe = launch_node(&mut world, KademliaConfig::default());

    // Bins 0 and 1 sit exactly at saturation, so one bounced bin-1 peer dips
    // the bin exactly one peer below the frontier: the marginal deficit the
    // lowering window exists to hold.
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
    assert!(converged, "fixture never converged (seed={SEED})");

    // Three flap cycles: bounce one frontier peer, hold the published depth
    // through the whole dip, confirm the peer is re-dialled. The peer has
    // served the node, so its drop is blameless churn, not a failing dial.
    for cycle in 0..3 {
        common::mark_productive(&handle, &scenario);
        world.bounce("bin1-0");
        let deadline = world.elapsed() + Duration::from_secs(8);
        while world.elapsed() < deadline {
            world
                .run_for(Duration::from_millis(100))
                .expect("world advances");
            assert_eq!(
                handle.routing_stats().depth,
                2,
                "a one-peer flap moved the published depth (cycle {cycle}, seed={SEED})"
            );
        }
        let recovered = run_until(
            &mut world,
            Duration::from_secs(120),
            Duration::from_millis(500),
            || connected_in_bin(&handle.routing_stats(), 1) == sat,
        );
        assert!(
            recovered,
            "the flapped peer was never re-dialled (cycle {cycle}, seed={SEED}): {:?}",
            handle.routing_stats()
        );
    }
    assert_eq!(handle.routing_stats().depth, 2, "depth held to the end");
}
