//! Readiness dip window: a neighbourhood sitting exactly at the saturation
//! threshold keeps its stability clock through a one-peer boundary flap,
//! while a genuine multi-peer loss clears the clock at once.

mod common;

use std::time::Duration;

use common::{await_handle, family, gossip, launch_node, place, run_until, spec};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_sim::{PeerScript, Scenario, SimWorld};
use vertex_swarm_topology::KademliaConfig;

const SEED: u64 = 17;
const STORER: SwarmNodeType = SwarmNodeType::Storer;

#[test]
fn boundary_flap_holds_the_readiness_clock() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(48 * 3600))
        .tick(Duration::from_millis(50))
        .latency(Duration::from_millis(10), Duration::from_millis(50))
        .tokio_io()
        .build();
    let probe = launch_node(&mut world, KademliaConfig::default());
    // Derive from the built world seed so a replay override reproduces exactly.
    let seed = world.seed();

    // Bin 0 saturated; the neighbourhood (bins >= 1) holds exactly the
    // saturation threshold: four bin-1 peers plus the flapper, two in bin 2,
    // one in bin 3. Every flap dips the count a single peer below threshold.
    let mut scenario = Scenario::new(&world, spec());
    let mut names = Vec::new();
    for (bin, count) in [(0u8, 8), (1, 4), (2, 2), (3, 1)] {
        for name in family(&format!("bin{bin}"), count) {
            scenario.add_peer(
                &mut world,
                &name,
                STORER,
                PeerScript::Honest,
                place(seed, bin),
            );
            names.push(name);
        }
    }
    scenario.add_peer(
        &mut world,
        "flapper",
        STORER,
        PeerScript::Honest,
        place(seed, 1),
    );
    names.push("flapper".to_owned());

    let (handle, _marker) = await_handle(&mut world, &probe);
    gossip(&handle, &mut scenario, &names);
    let converged = run_until(
        &mut world,
        Duration::from_secs(600),
        Duration::from_secs(2),
        || {
            let readiness = handle.readiness();
            readiness.depth.get() == 1
                && readiness.neighborhood_connected == 8
                && readiness.neighborhood_stable_for.is_some()
        },
    );
    assert!(
        converged,
        "the saturated neighbourhood never carried a readiness clock (seed={seed}): {:?}",
        handle.readiness()
    );

    // Flap cycles: each bounce dips the neighbourhood one peer below the
    // threshold until the re-dial restores it. The clock must hold through
    // every sample of every dip. A bounce that catches the flapper young
    // arms the early-disconnect backoff, which expires under virtual
    // advance inside the restore window.
    for cycle in 0..3 {
        world.bounce("flapper");
        let deadline = world.elapsed() + Duration::from_secs(6);
        while world.elapsed() < deadline {
            world
                .run_for(Duration::from_millis(100))
                .expect("world advances");
            let readiness = handle.readiness();
            assert_eq!(
                readiness.depth.get(),
                1,
                "a one-peer flap never moves depth (cycle {cycle}, seed={seed})"
            );
            assert!(
                readiness.neighborhood_stable_for.is_some(),
                "a one-peer flap never zeroes the readiness clock (cycle {cycle}, seed={seed})"
            );
        }
        let restored = run_until(
            &mut world,
            Duration::from_secs(120),
            Duration::from_millis(500),
            || handle.readiness().neighborhood_connected == 8,
        );
        assert!(
            restored,
            "the flapper was never re-dialled (cycle {cycle}, seed={seed}): {:?}",
            handle.readiness()
        );
    }

    // A genuine loss is still observed promptly: two neighbourhood peers
    // going away exceeds the dip tolerance and clears the clock with no
    // window.
    world.crash("bin2-0");
    world.crash("bin2-1");
    let observed = run_until(
        &mut world,
        Duration::from_secs(60),
        Duration::from_millis(100),
        || handle.readiness().neighborhood_connected <= 6,
    );
    assert!(
        observed,
        "the crashed pair was never observed (seed={seed})"
    );
    assert!(
        handle.readiness().neighborhood_stable_for.is_none(),
        "a multi-peer loss clears the readiness clock immediately (seed={seed})"
    );
}
