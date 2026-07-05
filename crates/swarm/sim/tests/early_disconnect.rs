//! Early-disconnect penalty: a peer that completes the handshake and drops
//! the connection quickly without serving the node takes the scoring penalty
//! and arms the dial backoff; advancing virtual time past the jittered window
//! admits the re-dial, and a second unproductive drop re-arms the backoff. A
//! peer that served the node is exempt and stays immediately re-dialable.
//!
//! The whole surface rides the runtime wall clock: connection age, the
//! productivity window, and backoff expiry all move under virtual advance.

mod common;

use std::time::Duration;

use common::{
    await_handle, gossip, launch_node, mark_overlays_productive, place, run_until, spec,
    trace_count,
};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_sim::{PeerScript, Scenario, SimWorld};
use vertex_swarm_topology::KademliaConfig;

const SEED: u64 = 11;
const STORER: SwarmNodeType = SwarmNodeType::Storer;

/// Scripted connection hold: well inside the 30s early-disconnect threshold,
/// so every scripted drop closes a young connection.
const HOLD: Duration = Duration::from_secs(5);

#[test]
#[allow(clippy::expect_used)]
fn early_disconnect_arms_backoff_until_virtual_expiry() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(48 * 3600))
        .tick(Duration::from_millis(50))
        .tokio_io()
        .build();
    let probe = launch_node(&mut world, KademliaConfig::default());
    // Derive from the built world seed so a replay override reproduces exactly.
    let seed = world.seed();

    // Both peers accept the handshake and cleanly close every connection
    // after at most HOLD; only the control ever serves the node.
    let mut scenario = Scenario::new(&world, spec());
    scenario.add_peer(
        &mut world,
        "dropper",
        STORER,
        PeerScript::AcceptThenDrop { hold: HOLD },
        place(seed, 0),
    );
    scenario.add_peer(
        &mut world,
        "control",
        STORER,
        PeerScript::AcceptThenDrop { hold: HOLD },
        place(seed, 1),
    );
    let dropper = scenario.peer("dropper").expect("registered").overlay;
    let control = scenario.peer("control").expect("registered").overlay;

    let (handle, marker) = await_handle(&mut world, &probe);
    gossip(
        &handle,
        &mut scenario,
        &["dropper".to_owned(), "control".to_owned()],
    );

    // Both scripted peers complete their handshakes.
    let pm = handle.peer_manager();
    let connected = run_until(
        &mut world,
        Duration::from_secs(60),
        Duration::from_millis(250),
        || pm.is_connected(&dropper) && pm.is_connected(&control),
    );
    assert!(
        connected,
        "the scripted peers never connected (seed={seed})"
    );

    // The control serves the node on every connection it holds during the
    // observation phase; the dropper never does.
    mark_overlays_productive(&marker, &[control]);
    let dropper_score = pm.get_peer_score(&dropper).expect("known peer");
    let control_attempts = trace_count(&world, "control", "established");

    // The scripted hold closes the young, unserved connection. The control
    // cycles too; keep each of its connections productive as it lands.
    let deadline = world.elapsed() + Duration::from_secs(30);
    let dropped = loop {
        if pm.is_connected(&control) {
            mark_overlays_productive(&marker, &[control]);
        }
        if !pm.is_connected(&dropper) {
            break true;
        }
        if world.elapsed() >= deadline {
            break false;
        }
        world
            .run_for(Duration::from_millis(250))
            .expect("world advances");
    };
    assert!(dropped, "the scripted drop never happened (seed={seed})");

    // The fast unproductive remote close lands the penalty and arms the
    // dial backoff; the productive control is exempt from both.
    assert!(
        pm.get_peer_score(&dropper).expect("known peer") < dropper_score,
        "an early disconnect must land a scoring penalty (seed={seed})"
    );
    assert!(
        pm.peer_is_in_backoff(&dropper),
        "an early disconnect must arm the dial backoff (seed={seed})"
    );
    assert!(
        !pm.peer_is_in_backoff(&control),
        "a productive connection is exempt from the penalty (seed={seed})"
    );

    // Under the minimum jittered window (22.5s at one failure) no re-dial of
    // the dropper is admitted, while the blameless control is re-dialled
    // immediately after its own drop: its second connection lands well
    // before any backoff could have expired, so none was armed.
    let dropper_attempts = trace_count(&world, "dropper", "established");
    let quiet_until = world.elapsed() + Duration::from_secs(15);
    while world.elapsed() < quiet_until {
        if pm.is_connected(&control) {
            mark_overlays_productive(&marker, &[control]);
        }
        world
            .run_for(Duration::from_millis(250))
            .expect("world advances");
    }
    assert_eq!(
        trace_count(&world, "dropper", "established"),
        dropper_attempts,
        "no re-dial is admitted while the backoff holds (seed={seed})"
    );
    assert!(
        pm.peer_is_in_backoff(&dropper),
        "the backoff holds through the whole armed window (seed={seed})"
    );
    assert!(
        trace_count(&world, "control", "established") > control_attempts,
        "a blameless drop must be re-dialled inside the backoff floor (seed={seed})"
    );

    // Past the maximum jittered window (37.5s at one failure) the evaluator
    // re-dials the dropper.
    let deadline = world.elapsed() + Duration::from_secs(120);
    let redialled = loop {
        if trace_count(&world, "dropper", "established") > dropper_attempts {
            break true;
        }
        if world.elapsed() >= deadline {
            break false;
        }
        world
            .run_for(Duration::from_millis(500))
            .expect("world advances");
    };
    assert!(
        redialled,
        "the expired backoff must admit a re-dial (seed={seed})"
    );

    // The re-dialled handshake completes and resets the failure counter.
    let reconnected = run_until(
        &mut world,
        Duration::from_secs(30),
        Duration::from_millis(250),
        || pm.is_connected(&dropper),
    );
    assert!(
        reconnected,
        "the re-dial never completed its handshake (seed={seed})"
    );
    assert!(
        !pm.peer_is_in_backoff(&dropper),
        "a completed handshake resets the dial backoff (seed={seed})"
    );

    // The second unproductive drop re-arms the backoff. The successful
    // handshake reset the failure counter, so the re-arm is the base window
    // again; consecutive-failure doubling without a reset is covered by the
    // starvation scenario.
    let rearmed = run_until(
        &mut world,
        Duration::from_secs(30),
        Duration::from_millis(250),
        || !pm.is_connected(&dropper) && pm.peer_is_in_backoff(&dropper),
    );
    assert!(
        rearmed,
        "a repeat early disconnect must re-arm the backoff (seed={seed})"
    );
}
