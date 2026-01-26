//! Type-state node builder for Vertex.
//!
//! This crate provides a type-safe builder for launching Vertex nodes.
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_builder::NodeBuilder;
//! use vertex_swarm_api::SwarmLightProtocol;
//!
//! let handle = NodeBuilder::<SwarmLightProtocol<MyConfig>>::new()
//!     .with_launch_context(executor, dirs, api_config)
//!     .with_protocol(protocol_config)
//!     .launch()
//!     .await?;
//!
//! handle.wait_for_exit().await?;
//! ```

mod builder;
mod handle;

pub use builder::*;
pub use handle::*;
