//! Handshake-failure release: a dial whose transport connects but whose
//! handshake fails releases its phase reservation, arms the dial backoff,
//! and leaks no per-bin counter.

mod common;

use std::time::Duration;

use common::{
    await_handle, connected_in_bin, family, gossip, launch_node, place, run_until, spec,
    trace_count,
};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_sim::{Invariants, PeerScript, Scenario, SimWorld};
use vertex_swarm_topology::KademliaConfig;

const SEED: u64 = 21;
const STORER: SwarmNodeType = SwarmNodeType::Storer;

#[test]
fn handshake_failure_releases_the_reservation() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(48 * 3600))
        .tick(Duration::from_millis(50))
        .tokio_io()
        .build();
    let probe = launch_node(&mut world, KademliaConfig::default());

    let mut scenario = Scenario::new(&world, spec());
    let mut names = Vec::new();
    for (bin, count) in [(0u8, 8), (1, 3)] {
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
    scenario.add_peer(
        &mut world,
        "failer",
        STORER,
        PeerScript::HandshakeFail,
        place(SEED, 1),
    );
    names.push("failer".to_owned());
    #[allow(clippy::expect_used)]
    let failer = scenario.peer("failer").expect("registered").overlay;

    let handle = await_handle(&mut world, &probe);
    gossip(&handle, &mut scenario, &names);

    // The dial reaches the failer's transport (the reservation is really
    // held), the handshake fails, and the failure arms the backoff.
    let deadline = world.elapsed() + Duration::from_secs(300);
    let failed = loop {
        if trace_count(&world, "failer", "established") >= 1
            && handle.peer_manager().peer_is_in_backoff(&failer)
        {
            break true;
        }
        if world.elapsed() >= deadline {
            break false;
        }
        world
            .run_for(Duration::from_secs(2))
            .expect("world advances");
    };
    assert!(
        failed,
        "the scripted dial never reached and failed the handshake (seed={SEED})"
    );

    // The honest supply converges around the failure.
    let converged = run_until(
        &mut world,
        Duration::from_secs(300),
        Duration::from_secs(2),
        || {
            let stats = handle.routing_stats();
            stats.depth == 1 && connected_in_bin(&stats, 1) == 3
        },
    );
    assert!(converged, "fixture never converged (seed={SEED})");

    // The failure never leaks into later rounds: the failer stays excluded
    // under backoff, only the honest peers count, and no dialing or
    // handshaking counter is left behind in any bin.
    let deadline = world.elapsed() + Duration::from_secs(120);
    while world.elapsed() < deadline {
        world
            .run_for(Duration::from_secs(2))
            .expect("world advances");
        let stats = handle.routing_stats();
        assert_eq!(
            connected_in_bin(&stats, 1),
            3,
            "only the honest peers count as connected (seed={SEED})"
        );
        for bin in &stats.bins {
            assert_eq!(
                (bin.dialing, bin.handshaking),
                (0, 0),
                "a settled table carries no phase reservation in bin {} (seed={SEED})",
                bin.bin
            );
        }
        Invariants::new()
            .phase_counters_consistent()
            .assert(&stats, SEED);
    }
    assert!(
        handle.peer_manager().peer_is_in_backoff(&failer),
        "the failer stays excluded under backoff (seed={SEED})"
    );
    assert_eq!(handle.routing_stats().connected_peers_total, 11);
}
