//! Swarm node CLI for Vertex.
//!
//! This crate provides Swarm-specific CLI entry points via the [`cli`] module.
//!
//! # CLI Usage
//!
//! The recommended way to build a Swarm node binary:
//!
//! ```ignore
//! use vertex_swarm_node::cli;
//! use vertex_swarm_builder::SwarmNodeBuilder;
//!
//! #[tokio::main]
//! async fn main() -> eyre::Result<()> {
//!     cli::run(|ctx, _args| async move {
//!         SwarmNodeBuilder::new(ctx)
//!             .launch()
//!             .await?
//!             .wait_for_shutdown()
//!             .await;
//!         Ok(())
//!     }).await
//! }
//! ```

pub mod cli;

pub use cli::{SwarmCli, SwarmCommands, SwarmNodeType, SwarmRunNodeArgs};

// Re-export from vertex-swarm-builder
pub use vertex_swarm_builder::{
    ClientNodeBuildConfig, DefaultClientTypes, DefaultNetworkConfig, SwarmLaunchContext,
    SwarmNodeBuilder, SwarmNodeError, create_and_save_signer, load_signer_from_keystore, node_type,
    resolve_password,
};
