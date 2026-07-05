//! Backoff starvation: when every known peer fails its dial, each candidate
//! arms the dial backoff after one attempt and the table reaches a
//! connection-level fixed point. Bookkeeping clocks ride the virtual
//! timeline, so the jittered backoff windows expire and re-dial attempts
//! resume with exponentially doubled re-arms, but this population can never
//! connect: the dark peers refuse the transport, and re-dials of the
//! handshake-failing peers abort against the connection the failed exchange
//! left open. This is the routing-level starvation the behaviour-layer
//! isolation probe exists to break.

mod common;

use std::time::Duration;

use common::{await_handle, gossip, launch_node, place, spec, trace_count};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_sim::{PeerScript, Scenario, SimWorld};
use vertex_swarm_topology::KademliaConfig;

const SEED: u64 = 3;
const STORER: SwarmNodeType = SwarmNodeType::Storer;

#[test]
fn all_backoff_is_starvation_without_redial() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(48 * 3600))
        .tick(Duration::from_millis(50))
        .tokio_io()
        .build();
    let probe = launch_node(&mut world, KademliaConfig::default());
    // Derive from the built world seed so a replay override reproduces exactly.
    let seed = world.seed();

    // Half the supply never listens (the dial fails at the transport), half
    // accepts the transport but fails the handshake; both paths arm backoff.
    // The handshake-failing hosts leave connection events in the trace, so
    // re-dial attempts are countable.
    let mut scenario = Scenario::new(&world, spec());
    let mut names = Vec::new();
    let mut countable = Vec::new();
    for bin in 0..3u8 {
        let dark = format!("dark-{bin}");
        scenario.add_peer(
            &mut world,
            &dark,
            STORER,
            PeerScript::Unreachable,
            place(seed, bin),
        );
        names.push(dark);
        let refuser = format!("refuser-{bin}");
        scenario.add_peer(
            &mut world,
            &refuser,
            STORER,
            PeerScript::HandshakeFail,
            place(seed, bin),
        );
        countable.push(refuser.clone());
        names.push(refuser);
    }

    let (handle, _marker) = await_handle(&mut world, &probe);
    let overlays: Vec<_> = scenario.peers().iter().map(|p| p.overlay).collect();
    gossip(&handle, &mut scenario, &names);

    // Every countable candidate is attempted at least once.
    let deadline = world.elapsed() + Duration::from_secs(300);
    let all_attempted = loop {
        if countable
            .iter()
            .all(|name| trace_count(&world, name, "established") >= 1)
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
        all_attempted,
        "every gossiped candidate must be dialled once (seed={seed})"
    );

    // The connection fixed point: across many further evaluation rounds and
    // several expired backoff windows, no re-dial ever reaches a countable
    // host again (the aborted attempts never touch its transport).
    world
        .run_for(Duration::from_secs(60))
        .expect("world advances");
    let attempts_before: Vec<usize> = countable
        .iter()
        .map(|name| trace_count(&world, name, "established"))
        .collect();
    world
        .run_for(Duration::from_secs(300))
        .expect("world advances");
    let attempts_after: Vec<usize> = countable
        .iter()
        .map(|name| trace_count(&world, name, "established"))
        .collect();
    assert_eq!(
        attempts_before, attempts_after,
        "an all-backoff table must not reconnect a failing candidate (seed={seed})"
    );
    // The dark half never even accepts a transport connection.
    for bin in 0..3u8 {
        assert_eq!(
            trace_count(&world, &format!("dark-{bin}"), "established"),
            0,
            "an unreachable candidate never sees a connection (seed={seed})"
        );
    }

    // Every candidate's failure count keeps the backoff armed: no dial ever
    // succeeded, so nothing reset it.
    assert!(
        overlays
            .iter()
            .all(|overlay| handle.peer_manager().peer_is_in_backoff(overlay)),
        "every failed candidate must arm the dial backoff (seed={seed})"
    );
    assert_eq!(
        handle.routing_stats().connected_peers_total,
        0,
        "no failing peer ever counts as connected"
    );
}
