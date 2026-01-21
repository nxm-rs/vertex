//! Vertex Swarm node binary
//!
//! This is the main entry point for the Vertex Swarm node.

#[tokio::main]
async fn main() -> color_eyre::eyre::Result<()> {
    vertex_node_core::run().await
}
