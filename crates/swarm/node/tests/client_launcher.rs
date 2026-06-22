//! Smoke test: [`ClientLauncher`] brings up an embedded client node and
//! returns working handles.
//!
//! Hermetic by construction: the spec carries no bootnodes and the launcher is
//! given none, so the spec fallback resolves to nothing and the node never
//! dials off-host. The point under test is the launch wiring (swarm assembly,
//! task spawning, handle plumbing), not connectivity; the cluster tests cover
//! the connected paths.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use eyre::Result;
use nectar_primitives::{AnyChunk, ContentChunk};
use vertex_swarm_api::{
    SwarmClientAccounting as _, SwarmIdentity as _, SwarmLocalStore as _, SwarmNodeType,
    SwarmTopologyStats as _,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::ClientLauncher;
use vertex_swarm_primitives::CachedChunk;
use vertex_swarm_spec::SpecBuilder;
use vertex_swarm_test_utils::TEST_NETWORK_ID;
use vertex_tasks::{TaskExecutor, TaskManager};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launcher_brings_up_client_node() -> Result<()> {
    // The launch path spawns onto the global TaskExecutor; install one if the
    // test process has not already done so. Held so the executor outlives the
    // launched node.
    let _task_manager = match TaskExecutor::try_current() {
        Ok(_) => None,
        Err(_) => Some(TaskManager::current()),
    };

    let spec = Arc::new(
        SpecBuilder::testnet()
            .network_id(TEST_NETWORK_ID)
            .bootnodes(Vec::new())
            .build(),
    );
    let identity = Identity::random(spec, SwarmNodeType::Client);
    let overlay = identity.overlay_address();

    let launched = ClientLauncher::new(identity)
        .with_max_peers(16)
        .launch()
        .await?;

    assert_eq!(launched.overlay_address(), overlay);
    assert_eq!(launched.topology().connected_peers_count(), 0);
    // The accessors hand out live handles: the peer id is the swarm's and the
    // client handle is clonable for embedding.
    let _peer_id = launched.local_peer_id();
    let _client = launched.client().clone();

    // The launcher now wires the shared client core: the accounting carries the
    // pseudosettle settlement mechanism instead of an empty provider list.
    let providers = launched.accounting().bandwidth().provider_names();
    assert!(
        providers.contains(&"pseudosettle"),
        "expected pseudosettle in the launched provider list, got {providers:?}"
    );

    // The in-memory client cache round-trips a content chunk.
    let chunk: AnyChunk = ContentChunk::new(&b"launcher core round trip"[..])
        .expect("valid content chunk")
        .into();
    let cached = CachedChunk::new(chunk, None);
    let address = *cached.address();
    launched.store().put(cached).expect("put");
    assert!(launched.store().contains(&address));

    Ok(())
}
