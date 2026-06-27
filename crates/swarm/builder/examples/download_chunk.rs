//! Build a Swarm client, wait until it can route, and download a chunk.
//!
//! The flow mirrors what an FFI or gRPC embedder does: launch a client through
//! the node-builder shell (`launch_without_grpc`), which spawns its event loop,
//! then dial bootnodes, await a deterministic readiness gate, and retrieve a
//! chunk by address.
//!
//! Readiness is `TopologyHandle::wait_until_ready`, the composite warm gate:
//! for a client it resolves the moment a storer is connected, the state from
//! which a retrieval has a peer to ask. It is event-driven, never a timed
//! guess. Stronger conditions (target depth, neighborhood saturation) are
//! available through `TopologyHandle::wait_until` and its shorthands.
//!
//! Run with: `cargo run -p vertex-swarm-builder --example download_chunk`

use std::sync::Arc;

use vertex_node_builder::NodeBuilder;
use vertex_node_core::dirs::DataDirs;
use vertex_swarm_api::{
    ChunkAddress, HasChunkClient, HasTopology, SwarmChunkProvider, SwarmNodeType,
    SwarmTopologyCommands,
};
use vertex_swarm_builder::{ChunkVerifyConfig, ClientConfig};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::{ChainConfig, NetworkConfig, SwapConfig};
use vertex_swarm_spec::init_dev;
use vertex_tasks::{TaskExecutor, TaskManager};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _manager = TaskManager::current();
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

    // Launch through the node-builder shell; the node task is spawned internally.
    let handle = NodeBuilder::new()
        .with_launch_context(
            (),
            executor,
            DataDirs::ephemeral(std::env::temp_dir().join("vertex-example-download")),
        )
        .with_protocol(config)
        .launch_without_grpc()
        .await?;
    let topology = handle.components().topology();

    // Dial bootnodes so peers can be discovered.
    topology.connect_bootnodes().await?;

    // Deterministic readiness: the warm gate for a client resolves once a
    // storer is connected.
    topology.wait_until_ready().await?;

    let address = ChunkAddress::new([0u8; 32]);
    println!("Retrieving chunk {address}");
    match handle
        .components()
        .chunk_client()
        .retrieve_chunk(&address)
        .await
    {
        Ok(result) => {
            println!("Served by {}", result.served_by);
            println!("Chunk address {}", result.chunk.address());
        }
        Err(err) => println!("Retrieval failed: {err}"),
    }

    Ok(())
}
