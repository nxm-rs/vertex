//! A whole client node runs over the simulated network through the
//! production transport seam and connects to a simulated bootnode.
//!
//! One whole-node host per test process: node background tasks spawn through
//! the process-global executor, which binds the runtime of the host that
//! installs it.
#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use libp2p::multiaddr::Protocol;
use vertex_swarm_api::{SwarmIdentity, SwarmTopologyCommands, SwarmTopologyStats};
use vertex_swarm_identity::Identity;
use vertex_swarm_net_handshake::{HandshakeBehaviour, HandshakeEvent, NoAddresses};
use vertex_swarm_node::ClientNode;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_sim::{
    HostContext, HostResult, SimAuth, SimNetworkConfig, SimWorld, host_keypair, host_nonce,
    host_signer, listen_multiaddr, transport_override,
};
use vertex_swarm_spec::Spec;
use vertex_swarm_test_utils::TEST_NETWORK_ID;
use vertex_tasks::{TaskExecutor, TaskManager};

const PORT: u16 = 1634;
const SEED: u64 = 21;

fn spec() -> Arc<Spec> {
    Arc::new(
        vertex_swarm_spec::SpecBuilder::testnet()
            .network_id(TEST_NETWORK_ID)
            .bootnodes(Vec::new())
            .build(),
    )
}

fn handshake_summary(event: &HandshakeEvent) -> String {
    match event {
        HandshakeEvent::Completed { peer_id, info, .. } => format!(
            "handshake-completed peer={peer_id} overlay={}",
            info.swarm_peer.overlay()
        ),
        HandshakeEvent::Failed { peer_id, error, .. } => {
            format!("handshake-failed peer={peer_id} error={error}")
        }
    }
}

/// Behaviour-level bootnode counterpart serving handshakes for the run.
async fn serve_bootnode(ctx: HostContext) -> HostResult {
    let identity = Arc::new(ctx.identity(spec(), SwarmNodeType::Storer));
    let swarm = ctx.swarm(SimAuth::Plaintext, move |_keypair| {
        HandshakeBehaviour::new(identity, Arc::new(NoAddresses), "sim")
    });
    let mut traced = ctx.traced(swarm, handshake_summary);
    traced.swarm_mut().listen_on(listen_multiaddr(PORT))?;
    traced.drive_for(Duration::from_secs(3600)).await;
    Ok(())
}

#[test]
fn client_node_connects_over_the_sim_transport() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(300))
        .tokio_io()
        .build();

    world.host("boot", serve_bootnode);
    let boot_addr = world.multiaddr_of("boot", PORT).with(Protocol::P2p(
        host_keypair(SEED, "boot").public().to_peer_id(),
    ));

    world.client("node", move |ctx| async move {
        // Bind the executor to this host's runtime before any node build.
        let task_manager = TaskManager::current();

        let identity = ctx.identity(spec(), SwarmNodeType::Client);
        let config = SimNetworkConfig::new(PORT, vec![boot_addr], 32);
        let (mut node, _service, _handle) = ClientNode::builder(identity)
            .with_transport(transport_override(SimAuth::Plaintext))
            .build(&config, None)
            .await
            .map_err(|e| format!("build client node: {e:#}"))?;
        node.start_listening()
            .map_err(|e| format!("start listening: {e:#}"))?;
        let topology = node.topology_handle().clone();

        let executor = TaskExecutor::current();
        let run =
            executor.spawn_with_graceful_shutdown_signal("sim-node", move |graceful| async move {
                let _ = node.run(graceful).await;
            });

        // The first bootnode dial can race the node's own listen address, so
        // re-issue until connected; virtual time makes the polling free.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let _ = topology.connect_bootnodes().await;
            if topology.connected_peers_count() > 0 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("node never connected to the sim bootnode".into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        ctx.record("node connected");

        // Dropping the manager fires the shutdown signal the run loop holds.
        drop(task_manager);
        tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .map_err(|_| "node did not shut down")?
            .map_err(|e| format!("node task: {e}"))?;
        Ok(())
    });

    world.run();

    let overlay = Identity::new(
        host_signer(SEED, "node"),
        host_nonce(SEED, "node"),
        spec(),
        SwarmNodeType::Client,
    )
    .overlay_address();
    let lines = world.trace_lines();
    assert!(
        lines.iter().any(|l| l.contains("boot")
            && l.contains("handshake-completed")
            && l.contains(&overlay.to_string())),
        "bootnode never completed a handshake with the node (seed={}): {lines:#?}",
        world.seed()
    );
}
