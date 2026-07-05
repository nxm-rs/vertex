//! A scripted population holds a whole client node's depth floor through
//! seeded churn bursts, with reusable invariants checked between steps.
//!
//! One whole-node host per test process: node background tasks spawn through
//! the process-global executor, which binds the runtime of the host that
//! installs it.
#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use vertex_swarm_api::{DEFAULT_SATURATION_PEERS, SwarmTopologyCommands};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::ClientNode;
use vertex_swarm_primitives::{Bin, SwarmNodeType, compute_overlay};
use vertex_swarm_sim::{
    Fault, FaultSchedule, Invariants, PeerScript, Placement, Probe, Scenario, SimAuth,
    SimNetworkConfig, SimWorld, host_signer, placement_nonce, transport_override,
};
use vertex_swarm_spec::Spec;
use vertex_swarm_test_utils::TEST_NETWORK_ID;
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::{TaskExecutor, TaskManager};

const PORT: u16 = 1634;
const SEED: u64 = 31;
const STORER: SwarmNodeType = SwarmNodeType::Storer;

fn spec() -> Arc<Spec> {
    Arc::new(
        vertex_swarm_spec::SpecBuilder::testnet()
            .network_id(TEST_NETWORK_ID)
            .bootnodes(Vec::new())
            .build(),
    )
}

/// Depth 1 needs bin 0 saturated and a low-watermark neighbourhood beyond
/// it; the scripted population supplies both, plus a cycling boundary peer
/// and an unreachable peer that must never count.
#[test]
fn churn_bursts_hold_the_depth_floor() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(3600))
        .tick(Duration::from_millis(50))
        .tokio_io()
        .build();

    // The node's overlay is derivable up front, so scripted peers can be
    // placed against it before the node exists.
    let node_spec = spec();
    let node_overlay = {
        use vertex_swarm_api::SwarmIdentity as _;
        Identity::new(
            host_signer(SEED, "node"),
            vertex_swarm_sim::host_nonce(SEED, "node"),
            node_spec.clone(),
            SwarmNodeType::Client,
        )
        .overlay_address()
    };
    let place = |bin: u8| {
        Some(Placement::new(
            node_overlay,
            Bin::new(bin).unwrap_or(Bin::MAX),
        ))
    };

    let saturation = usize::from(DEFAULT_SATURATION_PEERS);
    let mut scenario = Scenario::new(&world, spec());
    for idx in 0..saturation {
        scenario.add_peer(
            &mut world,
            &format!("bin0-{idx}"),
            STORER,
            PeerScript::Honest,
            place(0),
        );
    }
    // Three clustered peers anchor the neighbourhood at the low watermark.
    for idx in 0..3 {
        scenario.add_peer(
            &mut world,
            &format!("sybil-{idx}"),
            STORER,
            PeerScript::Sybil,
            place(1),
        );
    }
    scenario.add_peer(
        &mut world,
        "flapper",
        STORER,
        PeerScript::AcceptThenDrop {
            hold: Duration::from_secs(45),
        },
        place(1),
    );
    scenario.add_peer(
        &mut world,
        "dark",
        STORER,
        PeerScript::Unreachable,
        place(0),
    );

    let bootnodes = scenario.bootnodes();
    let probe: Probe<TopologyHandle<Identity>> = Probe::default();
    let publish = probe.clone();
    world.client("node", move |ctx| async move {
        // Bind the executor to this host's runtime before any node build.
        let _task_manager = TaskManager::current();

        let identity = ctx.identity(spec(), SwarmNodeType::Client);
        let config = SimNetworkConfig::new(PORT, bootnodes, 64)
            .with_idle_timeout(Duration::from_secs(24 * 3600));
        let (mut node, _service, _handle) = ClientNode::builder(identity)
            .with_transport(transport_override(SimAuth::Plaintext))
            .build(&config, None)
            .await
            .map_err(|e| format!("build client node: {e:#}"))?;
        node.start_listening()
            .map_err(|e| format!("start listening: {e:#}"))?;
        let topology = node.topology_handle().clone();
        publish.publish(topology.clone());

        let executor = TaskExecutor::current();
        let _run =
            executor.spawn_with_graceful_shutdown_signal("sim-node", move |graceful| async move {
                let _ = node.run(graceful).await;
            });

        // Re-dial for the whole run: already-tracked peers are skipped, so
        // this only re-establishes what churn tears down.
        loop {
            let _ = topology.connect_bootnodes().await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    // Converge: the node publishes its handle, then climbs to depth 1.
    let handle = loop {
        world
            .run_for(Duration::from_secs(5))
            .expect("world advances");
        if let Some(handle) = probe.get() {
            break handle;
        }
        assert!(
            world.elapsed() < Duration::from_secs(60),
            "node never published its topology handle (seed={SEED})"
        );
    };
    while handle.routing_stats().depth < 1 {
        assert!(
            world.elapsed() < Duration::from_secs(600),
            "depth never converged (seed={SEED}): {:?}",
            handle.routing_stats()
        );
        world
            .run_for(Duration::from_secs(5))
            .expect("world advances");
    }

    let always = Invariants::new()
        .phase_counters_consistent()
        .saturation_floor(saturation);
    let steady = Invariants::new().depth_floor(1);
    always.assert(&handle.routing_stats(), SEED);
    steady.assert(&handle.routing_stats(), SEED);

    // The unreachable peer never counts as connected.
    let stats = handle.routing_stats();
    assert_eq!(
        stats
            .bins
            .first()
            .map(|bin| bin.connected)
            .unwrap_or_default(),
        saturation,
        "bin 0 holds the honest supply and never the unreachable peer"
    );

    // Two seeded churn bursts: each bounces ~30% of the population; the
    // node re-dials the restarted peers and the depth floor returns.
    let start = world.elapsed();
    FaultSchedule::new()
        .at(
            start + Duration::from_secs(10),
            Fault::Churn { fraction: 0.3 },
        )
        .at(
            start + Duration::from_secs(150),
            Fault::Churn { fraction: 0.3 },
        )
        .apply(&mut world, &mut scenario, |world, _fault| {
            // Recovery window: poll until the depth floor returns.
            for _ in 0..24 {
                world
                    .run_for(Duration::from_secs(5))
                    .expect("world advances");
                if handle.routing_stats().depth >= 1 {
                    break;
                }
            }
            let stats = handle.routing_stats();
            always.assert(&stats, SEED);
            steady.assert(&stats, SEED);
        })
        .expect("schedule applies");
}

/// The placement grinder and the scripted identity agree: a peer placed in
/// bin 2 of an anchor really lands there with the identity the host derives.
#[test]
fn placement_matches_the_hosted_identity() {
    use vertex_swarm_api::SwarmSpec as _;

    let spec = spec();
    let anchor = compute_overlay(
        &host_signer(SEED, "anchor").address(),
        spec.network_id(),
        &vertex_swarm_sim::host_nonce(SEED, "anchor"),
    );
    let placement = Placement::new(anchor, Bin::new(2).unwrap_or(Bin::MAX));
    let nonce = placement_nonce(SEED, "placed", &spec, placement);
    let overlay = compute_overlay(
        &host_signer(SEED, "placed").address(),
        spec.network_id(),
        &nonce,
    );
    assert_eq!(anchor.proximity(&overlay).get(), 2);
}

/// The `AcceptThenDrop` script does more than name a variant: a dialer that
/// completes the handshake is observed being cleanly dropped once the hold
/// elapses, so the fault the schedule leans on actually fires on the wire.
#[test]
fn accept_then_drop_is_observed_dropping() {
    use libp2p::swarm::SwarmEvent;
    use vertex_swarm_net_handshake::{HandshakeBehaviour, HandshakeEvent, NoAddresses};
    use vertex_swarm_sim::handshake_summary;

    const HOLD: Duration = Duration::from_secs(5);

    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(120))
        .build();

    let mut scenario = Scenario::new(&world, spec());
    scenario.add_peer(
        &mut world,
        "flapper",
        STORER,
        PeerScript::AcceptThenDrop { hold: HOLD },
        None,
    );
    let target = scenario
        .peers()
        .first()
        .expect("one scripted peer")
        .multiaddr
        .clone();

    world.client("dialer", move |ctx| async move {
        let identity = Arc::new(ctx.identity(spec(), SwarmNodeType::Client));
        let swarm = ctx.swarm(SimAuth::Plaintext, move |_keypair| {
            HandshakeBehaviour::new(identity, Arc::new(NoAddresses), "sim")
        });
        let mut traced = ctx.traced(swarm, handshake_summary);
        // Fixed virtual delay so the scripted peer is always listening first.
        tokio::time::sleep(Duration::from_millis(200)).await;
        traced.swarm_mut().dial(target)?;

        let completed = traced
            .drive_until(Duration::from_secs(30), |event| {
                matches!(
                    event,
                    SwarmEvent::Behaviour(HandshakeEvent::Completed { .. })
                )
            })
            .await;
        if !completed {
            return Err("handshake never completed against the scripted peer".into());
        }

        // The peer holds the connection for `HOLD`, then closes it cleanly.
        let dropped = traced
            .drive_until(HOLD + Duration::from_secs(30), |event| {
                matches!(event, SwarmEvent::ConnectionClosed { .. })
            })
            .await;
        if !dropped {
            return Err("scripted peer never dropped the connection".into());
        }
        Ok(())
    });

    world.run();
}
