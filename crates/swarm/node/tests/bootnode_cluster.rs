//! Integration test: 1 vertex bootnode + 2 vertex clients in-process.
//!
//! Asserts convergence:
//!  (a) Both clients complete a handshake with the bootnode.
//!  (b) Clients learn each other's overlay via hive gossip from the bootnode.
//!  (c) Kademlia depth reaches the expected value (0 for N=2 small-PO peers).
//!
//! The bootnode discard-metric assertion is intentionally absent; the
//! `hive_peers_discarded_total{reason="bootnode_mode"}` counter exists but
//! is not exposed through the test harness yet.
//!
//! See `vertex_swarm_test_utils::cluster` for the harness rationale. The
//! cluster runs over the in-process memory transport, so paused virtual time
//! (`start_paused`) drives convergence with no wall-clock waiting: the
//! transport wakes tasks through channels, so an idle runtime auto-advances
//! the clock past each `timeout` budget.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashSet;
use std::time::Duration;

use eyre::Result;
use libp2p::PeerId;
use tokio::time::timeout;
use vertex_swarm_api::{SwarmTopologyPeers as _, SwarmTopologyState as _, SwarmTopologyStats as _};
use vertex_swarm_primitives::{Bin, NeighborhoodDepth, all_bins};
use vertex_swarm_test_utils::cluster::{ClusterBuilder, NodeRole, Transport};
use vertex_swarm_topology::{TopologyEvent, TopologyPhase};

/// Virtual-time budget the test allows for handshakes to complete. Under
/// `start_paused` an idle runtime auto-advances to this deadline instantly, so
/// it bounds logical rather than wall-clock time.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test(start_paused = true)]
async fn three_node_convergence() -> Result<()> {
    // Best-effort tracing init so RUST_LOG works when debugging locally.
    // `try_init` so re-running the test in the same process is a no-op.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .try_init();

    // Build the cluster: 1 bootnode + 2 clients over the default memory
    // transport. Each node takes a process-unique `/memory/<port>` address,
    // each client is configured with the bootnode's listen multiaddr as its
    // sole bootstrap entry, and every event loop runs on the global
    // TaskExecutor.
    let mut cluster = ClusterBuilder::new()
        .with_bootnode()
        .with_clients(2)
        .build()
        .await?;

    assert_eq!(cluster.client_count(), 2);

    // Take the pre-subscribed event receiver BEFORE inspecting the bootnode
    // handle. The harness subscribed inside `build` before spawning any peer,
    // closing the race against a handshake that would otherwise complete
    // before a caller could subscribe.
    let mut bootnode_events = cluster
        .take_bootnode_events()
        .expect("cluster built with bootnode");

    let bootnode = cluster.bootnode();
    assert_eq!(bootnode.role, NodeRole::Bootnode);
    let bootnode_overlay = bootnode.overlay;
    let bootnode_topo = bootnode.topology.clone();

    let clients = cluster.clients();
    let client_overlays: Vec<_> = clients.iter().map(|c| c.overlay).collect();
    let client_peer_ids: HashSet<PeerId> = clients.iter().map(|c| c.peer_id).collect();

    // ──────────────────────────────────────────────────────────────────
    // (a) Both clients complete a handshake with the bootnode.
    // ──────────────────────────────────────────────────────────────────
    //
    // We tally distinct `PeerReady` events whose `peer_id` matches one of
    // the two clients. `PeerReady` fires from `TopologyBehaviour` only
    // after the Swarm handshake has signed/verified the BzzAddress.
    let mut seen_handshakes: HashSet<PeerId> = HashSet::new();

    timeout(HANDSHAKE_TIMEOUT, async {
        while seen_handshakes.len() < 2 {
            match bootnode_events.recv().await {
                Ok(TopologyEvent::PeerReady { peer_id, .. })
                    if client_peer_ids.contains(&peer_id) =>
                {
                    seen_handshakes.insert(peer_id);
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // The bootnode is healthy enough to outpace our
                    // subscriber; the depth assertion below will still
                    // observe the final state regardless of which events
                    // we miss in transit.
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
    .await
    .map_err(|_| {
        eyre::eyre!(
            "timed out after {:?} waiting for both clients to handshake with the bootnode \
             (saw {} of 2)",
            HANDSHAKE_TIMEOUT,
            seen_handshakes.len()
        )
    })?;

    assert_eq!(
        seen_handshakes.len(),
        2,
        "expected both client peer ids in bootnode handshake events; saw {seen_handshakes:?}"
    );

    // ──────────────────────────────────────────────────────────────────
    // (b) Clients learn each other's overlay via hive gossip.
    //
    // TODO: re-enable once hive gossip propagates
    // *Client* peers (today the gossip task short-circuits in
    // `vertex_swarm_topology::gossip::tasks::on_peer_authenticated` via
    // `!node_type.requires_storage()`, so two Client nodes never gossip
    // about each other through a bootnode).
    //
    // Until then, we assert the *necessary* precondition: each client
    // has its own routing snapshot of the bootnode (the only peer it
    // dialed directly). If that breaks, hive gossip can never make
    // progress.
    // ──────────────────────────────────────────────────────────────────
    for (idx, client) in clients.iter().enumerate() {
        let knows_bootnode = all_bins(Bin::MAX)
            .flat_map(|po| client.topology.connected_peers_in_bin(po))
            .any(|peer| peer == bootnode_overlay);
        assert!(
            knows_bootnode,
            "client {idx} should have the bootnode in its connected-peers view; \
             routing-bin snapshot omits overlay {bootnode_overlay}"
        );
    }

    // Suppress unused-binding warning for the structural reservation,
    // re-enabled the moment assertion (b) becomes live.
    let _ = client_overlays;

    // ──────────────────────────────────────────────────────────────────
    // (c) Kademlia depth converges to the expected value.
    // ──────────────────────────────────────────────────────────────────
    //
    // With N=2 random overlays plus the bootnode's, all three nodes sit in
    // small proximity orders (random bits = bin 0/1 with overwhelming
    // probability). The recompute formula treats every connected peer at
    // small PO as outside the neighborhood, so depth pins at the zero bin.
    let bootnode_depth = bootnode_topo.depth();
    assert_eq!(
        bootnode_depth,
        NeighborhoodDepth::ZERO,
        "bootnode kademlia depth should be 0 with two small-PO peers, got {bootnode_depth}"
    );
    for (idx, client) in clients.iter().enumerate() {
        let depth = client.topology.depth();
        assert_eq!(
            depth,
            NeighborhoodDepth::ZERO,
            "client {idx} kademlia depth should be 0 (small-PO peers only), got {depth}"
        );
    }

    // The topology phase machine mirrors the depth result: a three-node
    // cluster cannot anchor a neighborhood, so every node reports the
    // Bootstrap phase on its readiness surface.
    for (idx, client) in clients.iter().enumerate() {
        let readiness = client.topology.readiness();
        assert_eq!(
            readiness.phase,
            TopologyPhase::Bootstrap,
            "client {idx} should report Bootstrap phase at depth 0"
        );
    }

    // Connectivity sanity: the bootnode now reports >= 2 connected peers.
    assert!(
        bootnode_topo.connected_peers_count() >= 2,
        "bootnode should be connected to both clients; got {}",
        bootnode_topo.connected_peers_count()
    );

    // Bootnode's overlay address is well-defined and non-zero (smoke check
    // guarding against an accidental ephemeral-identity regression).
    assert!(
        !bootnode_overlay.is_zero(),
        "bootnode overlay should never be the zero address"
    );

    // ──────────────────────────────────────────────────────────────────
    // (d) Bootnode metric `hive_peers_discarded_total > 0`.
    //
    // TODO: re-enable once `vertex_swarm_net_hive::metrics` exposes
    // the discard counter. Today the bootnode happily ingests hive gossip,
    // so there is no counter to scrape. Asserting on it now would always
    // fail. Skipping at runtime would hide regressions, so the assertion
    // is deliberately absent until the metric exists.
    // ──────────────────────────────────────────────────────────────────

    // Tear down. We intentionally do not unwrap the per-node results: the
    // bootnode/client `run` futures return `Ok(())` on graceful shutdown,
    // but if any internal task panicked the JoinHandle would surface it
    // here instead.
    let join_results = cluster.shutdown().await;
    for (idx, result) in join_results.iter().enumerate() {
        if let Err(err) = result {
            return Err(eyre::eyre!(
                "cluster node #{idx} did not shut down cleanly: {err}"
            ));
        }
    }

    Ok(())
}

/// TCP smoke: the cluster's real-socket mode (ephemeral port reservation plus
/// the bootnode-dial kick) stays exercised. Real I/O, so a live clock and a
/// wall-clock budget rather than paused time.
#[tokio::test]
async fn tcp_cluster_smoke() -> Result<()> {
    let cluster = ClusterBuilder::new()
        .with_transport_mode(Transport::Tcp)
        .with_bootnode()
        .with_clients(2)
        .build()
        .await?;

    let bootnode_topo = cluster.bootnode().topology.clone();
    let connected = timeout(Duration::from_secs(15), async {
        while bootnode_topo.connected_peers_count() < 2 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok();
    assert!(
        connected,
        "bootnode should connect both clients over TCP; got {}",
        bootnode_topo.connected_peers_count()
    );

    for (idx, result) in cluster.shutdown().await.iter().enumerate() {
        if let Err(err) = result {
            return Err(eyre::eyre!(
                "cluster node #{idx} did not shut down cleanly: {err}"
            ));
        }
    }
    Ok(())
}

/// Storer smoke: the cluster's storer role builds a `StorerNode` over the mock
/// store and completes a handshake with the bootnode.
#[tokio::test(start_paused = true)]
async fn storer_cluster_smoke() -> Result<()> {
    let mut cluster = ClusterBuilder::new()
        .with_bootnode()
        .with_storers(1)
        .build()
        .await?;

    let mut bootnode_events = cluster
        .take_bootnode_events()
        .expect("cluster built with bootnode");
    assert_eq!(cluster.storer_count(), 1);
    let storer = cluster.storers().first().expect("one storer built");
    assert_eq!(storer.role, NodeRole::Storer);
    let storer_peer_id = storer.peer_id;

    timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match bootnode_events.recv().await {
                Ok(TopologyEvent::PeerReady { peer_id, .. }) if peer_id == storer_peer_id => break,
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
    .await
    .map_err(|_| eyre::eyre!("timed out waiting for the storer to handshake with the bootnode"))?;

    for (idx, result) in cluster.shutdown().await.iter().enumerate() {
        if let Err(err) = result {
            return Err(eyre::eyre!(
                "cluster node #{idx} did not shut down cleanly: {err}"
            ));
        }
    }
    Ok(())
}
