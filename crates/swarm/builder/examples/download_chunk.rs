//! Build a Swarm client, wait until it can route, and download a chunk.
//!
//! The flow mirrors what an FFI or gRPC embedder does: build a client through
//! `DefaultClientBuilder`, spawn its event loop, dial bootnodes, await a
//! deterministic readiness gate, then retrieve a chunk by address.
//!
//! Readiness is `TopologyHandle::wait_until_ready`, the composite warm gate:
//! for a client it resolves the moment a storer is connected, the state from
//! which a retrieval has a peer to ask. It is event-driven, never a timed
//! guess. Stronger conditions (target depth, neighborhood saturation) are
//! available through `TopologyHandle::wait_until` and its shorthands.
//!
//! Run with: `cargo run -p vertex-swarm-builder --example download_chunk`

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vertex_node_api::InfrastructureContext;
use vertex_swarm_api::{
    ChunkAddress, HasChunkClient, HasTopology, SwarmChunkProvider, SwarmNodeType,
    SwarmTopologyCommands,
};
use vertex_swarm_builder::{ChunkVerifyConfig, ClientConfig, DefaultClientBuilder};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::{ChainConfig, NetworkConfig, SwapConfig};
use vertex_swarm_spec::init_dev;
use vertex_tasks::TaskExecutor;

/// Minimal [`InfrastructureContext`]: a task executor for the client's
/// services and a data directory. No database path is configured, so the
/// client runs fully in-memory.
struct ExampleContext {
    executor: TaskExecutor,
    data_dir: PathBuf,
}

impl InfrastructureContext for ExampleContext {
    fn executor(&self) -> &TaskExecutor {
        &self.executor
    }

    fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executor = TaskExecutor::current();
    let spec = init_dev();
    let identity = Arc::new(Identity::random(spec.clone(), SwarmNodeType::Client));

    let verify = ChunkVerifyConfig {
        verify_content: true,
        verify_stamp: false,
    };
    let config = ClientConfig::new(
        spec,
        identity,
        NetworkConfig::default(),
        Default::default(),
        Default::default(),
        verify,
        ChainConfig::default(),
        SwapConfig::default(),
    );
    let ctx = ExampleContext {
        executor: executor.clone(),
        data_dir: std::env::temp_dir().join("vertex-example-download"),
    };

    let (task_fn, providers) = DefaultClientBuilder::from_config(config)
        .build(&ctx)
        .await?
        .into_parts();

    // Spawn the event loop and dial bootnodes so peers can be discovered.
    executor.spawn_critical_with_graceful_shutdown_signal("swarm.node", task_fn);
    providers.topology().connect_bootnodes().await?;

    // Deterministic readiness: the warm gate for a client resolves once a
    // storer is connected.
    providers.topology().wait_until_ready().await?;

    let address = ChunkAddress::new([0u8; 32]);
    println!("Retrieving chunk {address}");
    match providers.chunk_client().retrieve_chunk(&address).await {
        Ok(result) => {
            println!("Served by {}", result.served_by);
            println!("Chunk address {}", result.chunk.address());
        }
        Err(err) => println!("Retrieval failed: {err}"),
    }

    Ok(())
}
