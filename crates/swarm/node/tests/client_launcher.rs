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
use nectar_primitives::{AnyChunk, ChunkAddress, ContentChunk};
use vertex_swarm_api::{
    OverlayAddress, SwarmChunkProvider as _, SwarmClientAccounting as _, SwarmError,
    SwarmIdentity as _, SwarmLocalStore as _, SwarmNodeType, SwarmTopologyStats as _,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::{ClientLauncher, LaunchedClient};
use vertex_swarm_primitives::CachedChunk;
use vertex_swarm_spec::SpecBuilder;
use vertex_swarm_test_utils::TEST_NETWORK_ID;
use vertex_tasks::{TaskExecutor, TaskManager};

/// Bring up a hermetic (no-bootnode) native client, returning it and its overlay.
async fn hermetic_client() -> Result<(LaunchedClient, OverlayAddress)> {
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
    Ok((launched, overlay))
}

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrieve_serves_a_cached_content_chunk_without_racing_the_swarm() -> Result<()> {
    let _task_manager = match TaskExecutor::try_current() {
        Ok(_) => None,
        Err(_) => Some(TaskManager::current()),
    };
    let (launched, overlay) = hermetic_client().await?;

    // Seed the node's own cache with a content chunk, then retrieve it: the
    // provider serves the cached bytes and never races the peerless swarm.
    let chunk: AnyChunk = ContentChunk::new(&b"cache-read serves locally"[..])
        .expect("valid content chunk")
        .into();
    let cached = CachedChunk::new(chunk.clone(), None);
    let address = *cached.address();
    launched.store().put(cached).expect("put");

    let result = launched
        .chunks()
        .retrieve_chunk(&address)
        .await
        .expect("the cached chunk is served");
    assert_eq!(
        result.chunk, chunk,
        "the cached bytes are returned unchanged"
    );
    assert!(
        result.stamp.is_none(),
        "a content chunk is cached stampless"
    );
    assert_eq!(
        result.served_by, overlay,
        "a cache hit is marked served by our own overlay, not a peer"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrieve_misses_fall_through_to_the_engine() -> Result<()> {
    let _task_manager = match TaskExecutor::try_current() {
        Ok(_) => None,
        Err(_) => Some(TaskManager::current()),
    };
    let (launched, _overlay) = hermetic_client().await?;

    // An address the cache does not hold falls through to the retrieval engine,
    // which has no connected peers to race and so exhausts at once.
    let missing = ChunkAddress::new([0x7c; 32]);
    let outcome = launched.chunks().retrieve_chunk(&missing).await;
    assert!(
        matches!(outcome, Err(SwarmError::RetrievalExhausted { .. })),
        "a cache miss races the swarm and exhausts with no peers, got {outcome:?}"
    );

    Ok(())
}
