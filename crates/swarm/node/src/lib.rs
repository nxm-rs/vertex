//! Swarm node CLI for Vertex.
//!
//! This crate provides:
//! - Swarm-specific CLI entry points via the [`cli`] module
//! - Node presets (SwarmLightNode, SwarmFullNode, SwarmPublisherNode)
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
mod full;
mod light;
mod publisher;

pub use cli::{SwarmCli, SwarmCommands, SwarmNodeType, SwarmRunNodeArgs};
pub use full::SwarmFullNode;
pub use light::SwarmLightNode;
pub use publisher::SwarmPublisherNode;

// Re-export from vertex-swarm-builder
pub use vertex_swarm_builder::{
    DefaultLightTypes, DefaultNetworkConfig, LightNodeBuildConfig, SwarmLaunchContext,
    SwarmNodeBuilder, SwarmNodeError, create_and_save_signer, load_signer_from_keystore, node_type,
    resolve_password,
};
