//! Shared scaffolding for whole-node kademlia scenarios.
//!
//! Each scenario runs one client node against a scripted population, feeds
//! the node signed gossip records through its live peer manager, and reads
//! routing statistics between world steps. One whole-node host per test
//! process: node background tasks spawn through the process-global executor.

#![allow(dead_code)]
#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use vertex_swarm_api::SwarmIdentity as _;
use vertex_swarm_identity::Identity;
use vertex_swarm_node::ClientNode;
use vertex_swarm_primitives::{Bin, OverlayAddress, SwarmNodeType};
use vertex_swarm_sim::{
    Placement, Probe, Scenario, SimAuth, SimNetworkConfig, SimWorld, host_nonce, host_signer,
    transport_override,
};
use vertex_swarm_spec::Spec;
use vertex_swarm_test_utils::TEST_NETWORK_ID;
use vertex_swarm_topology::{KademliaConfig, RoutingStats, TopologyHandle};
use vertex_tasks::{TaskExecutor, TaskManager};

/// Port the node listens on inside the world.
pub(crate) const NODE_PORT: u16 = 1634;

/// Host name of the node under test.
pub(crate) const NODE: &str = "node";

/// Test spec: testnet parameters on the test network id, no bootnodes.
pub(crate) fn spec() -> Arc<Spec> {
    Arc::new(
        vertex_swarm_spec::SpecBuilder::testnet()
            .network_id(TEST_NETWORK_ID)
            .bootnodes(Vec::new())
            .build(),
    )
}

/// The node's overlay, derivable before the node exists so scripted peers
/// can be placed against it.
pub(crate) fn node_overlay(seed: u64) -> OverlayAddress {
    Identity::new(
        host_signer(seed, NODE),
        host_nonce(seed, NODE),
        spec(),
        SwarmNodeType::Client,
    )
    .overlay_address()
}

/// Placement of a scripted peer in `bin` relative to the node.
pub(crate) fn place(seed: u64, bin: u8) -> Option<Placement> {
    Some(Placement::new(
        node_overlay(seed),
        Bin::new(bin).unwrap_or(Bin::MAX),
    ))
}

/// Register the node under test; its topology handle arrives on the probe
/// once the node is built and listening. The host future never completes,
/// so the world keeps stepping for the whole scenario.
pub(crate) fn launch_node(
    world: &mut SimWorld,
    routing: KademliaConfig,
) -> Probe<TopologyHandle<Identity>> {
    let probe: Probe<TopologyHandle<Identity>> = Probe::default();
    let publish = probe.clone();
    world.client(NODE, move |ctx| async move {
        // Bind the executor to this host's runtime before any node build.
        let _task_manager = TaskManager::current();

        let identity = ctx.identity(spec(), SwarmNodeType::Client);
        let config = SimNetworkConfig::new(NODE_PORT, Vec::new(), 128)
            .with_idle_timeout(Duration::from_secs(24 * 3600));
        let (mut node, _service, _handle) = ClientNode::builder(identity)
            .with_kademlia_config(routing)
            .with_transport(transport_override(SimAuth::Plaintext))
            .build(&config, None)
            .await
            .map_err(|e| format!("build client node: {e:#}"))?;
        node.start_listening()
            .map_err(|e| format!("start listening: {e:#}"))?;
        publish.publish(node.topology_handle().clone());

        let executor = TaskExecutor::current();
        let _run =
            executor.spawn_with_graceful_shutdown_signal("sim-node", move |graceful| async move {
                let _ = node.run(graceful).await;
            });

        futures::future::pending::<()>().await;
        Ok(())
    });
    probe
}

/// Step the world until the node publishes its topology handle.
pub(crate) fn await_handle(
    world: &mut SimWorld,
    probe: &Probe<TopologyHandle<Identity>>,
) -> TopologyHandle<Identity> {
    let deadline = world.elapsed() + Duration::from_secs(60);
    loop {
        if let Some(handle) = probe.get() {
            return handle;
        }
        assert!(
            world.elapsed() < deadline,
            "node never published its topology handle (seed={})",
            world.seed()
        );
        world
            .run_for(Duration::from_millis(500))
            .expect("world advances");
    }
}

/// Feed the node a signed gossip record for each named scripted peer, as
/// hive discovery would.
pub(crate) fn gossip(handle: &TopologyHandle<Identity>, scenario: &mut Scenario, names: &[String]) {
    for name in names {
        handle
            .peer_manager()
            .store_discovered_peer(scenario.signed_record(name));
    }
}

/// Mark connected scripted peers as having served the node, as a completed
/// retrieval would.
///
/// A productive connection is exempt from the early-disconnect penalty, so a
/// later scripted drop reads as blameless churn (the peer stays re-dialable)
/// rather than as a failing dial that arms backoff.
pub(crate) fn mark_productive(handle: &TopologyHandle<Identity>, scenario: &Scenario) {
    let overlays: Vec<OverlayAddress> = scenario.peers().iter().map(|p| p.overlay).collect();
    mark_overlays_productive(handle, &overlays);
}

/// [`mark_productive`] over an explicit overlay list.
pub(crate) fn mark_overlays_productive(
    handle: &TopologyHandle<Identity>,
    overlays: &[OverlayAddress],
) {
    use vertex_swarm_api::{ReportSource, SwarmScoringEvent};
    for overlay in overlays {
        handle.peer_manager().report_peer(
            overlay,
            SwarmScoringEvent::RetrievalSuccess {
                latency: Duration::from_millis(50),
            },
            ReportSource::Protocol("retrieval"),
        );
    }
}

/// Step the world in `step` increments until `done` holds or `timeout` of
/// virtual time passes, returning whether it held.
pub(crate) fn run_until(
    world: &mut SimWorld,
    timeout: Duration,
    step: Duration,
    mut done: impl FnMut() -> bool,
) -> bool {
    let deadline = world.elapsed() + timeout;
    loop {
        if done() {
            return true;
        }
        if world.elapsed() >= deadline {
            return false;
        }
        world.run_for(step).expect("world advances");
    }
}

/// Connected peers in `bin` from a stats snapshot.
pub(crate) fn connected_in_bin(stats: &RoutingStats, bin: u8) -> usize {
    stats
        .bins
        .get(bin as usize)
        .map(|b| b.connected)
        .unwrap_or_default()
}

/// Count of trace lines at `host` starting with `prefix`.
pub(crate) fn trace_count(world: &SimWorld, host: &str, prefix: &str) -> usize {
    world
        .trace_entries()
        .iter()
        .filter(|e| e.host == host && e.line.starts_with(prefix))
        .count()
}

/// Names of a generated peer family: `prefix-0..prefix-<count>`.
pub(crate) fn family(prefix: &str, count: usize) -> Vec<String> {
    (0..count).map(|i| format!("{prefix}-{i}")).collect()
}
