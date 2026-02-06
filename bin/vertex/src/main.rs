//! Vertex Swarm node binary.

mod cli;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    cli::run().await
}
