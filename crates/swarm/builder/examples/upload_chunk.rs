//! Build a Swarm client, wait until it can route, and upload a stamped chunk.
//!
//! The flow mirrors what an FFI or gRPC embedder does: launch a client through
//! the node-builder shell (`launch_without_grpc`), which spawns its event loop,
//! then dial bootnodes, await a deterministic readiness gate, and push a
//! pre-stamped chunk.
//!
//! Readiness is `TopologyHandle::wait_until_ready`, the composite warm gate:
//! for a client it resolves the moment a storer is connected, the state from
//! which a push can reach the closest storers. It is event-driven, never a
//! timed guess. Stronger conditions (target depth, neighborhood saturation)
//! are available through `TopologyHandle::wait_until` and its shorthands.
//!
//! Run with: `cargo run -p vertex-swarm-builder --example upload_chunk`

use std::sync::Arc;

use alloy_primitives::B256;
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use nectar_postage::{Stamp, StampDigest, StampIndex};
use vertex_node_builder::NodeBuilder;
use vertex_node_core::dirs::DataDirs;
use vertex_swarm_api::{
    AnyChunk, Chunk, ChunkAddress, ContentChunk, HasChunkClient, HasTopology, StampedChunk,
    SwarmChunkSender, SwarmNodeType, SwarmTopologyCommands,
};
use vertex_swarm_builder::ClientConfig;
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::{ChainConfig, NetworkConfig, SwapConfig};
use vertex_swarm_spec::init_dev;
use vertex_tasks::{TaskExecutor, TaskManager};

/// Sign a stamp that recovers to a deterministic key, so the example needs no
/// wallet or chain access. A real uploader signs with the key owning an on-chain
/// postage batch.
fn sign_stamp(address: &ChunkAddress) -> Result<Stamp, Box<dyn std::error::Error>> {
    let signer = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x11))?;
    let batch = B256::repeat_byte(0x22);
    let index = StampIndex::new(0, 0);
    let timestamp = 1_700_000_000u64;
    let prehash = StampDigest::new(*address, batch, index, timestamp).to_prehash();
    let sig = signer.sign_message_sync(prehash.as_slice())?;
    Ok(Stamp::with_index(batch, index, timestamp, sig))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _manager = TaskManager::current();
    let executor = TaskExecutor::current();
    let spec = init_dev();
    let identity = Arc::new(Identity::random(spec.clone(), SwarmNodeType::Client));

    let config = ClientConfig::new(
        spec,
        identity,
        NetworkConfig::default(),
        Default::default(),
        Default::default(),
        ChainConfig::default(),
        SwapConfig::default(),
    );

    // Launch through the node-builder shell; the node task is spawned internally.
    let handle = NodeBuilder::new()
        .with_launch_context(
            (),
            executor,
            DataDirs::ephemeral(std::env::temp_dir().join("vertex-example-upload")),
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

    let chunk = ContentChunk::new(&b"hello swarm"[..])?;
    let address = *chunk.address();
    let stamped = StampedChunk::new(AnyChunk::Content(chunk), sign_stamp(&address)?);

    println!("Uploading chunk {address}");
    let receipt = handle
        .components()
        .chunk_client()
        .send_chunk(stamped)
        .await?;
    println!("Accepted by storer {}", receipt.storer);
    println!("Signature {}", receipt.signature);
    println!("Storage radius {}", receipt.storage_radius);

    Ok(())
}
