//! Type-state node builder for Vertex.
//!
//! This crate provides a type-safe builder for launching Vertex nodes.
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_builder::NodeBuilder;
//!
//! let handle = NodeBuilder::new()
//!     .with_launch_context(executor, dirs, api_config)
//!     .with_protocol(my_config)  // Protocol inferred from config type
//!     .launch()
//!     .await?;
//!
//! handle.wait_for_exit().await?;
//! ```

mod builder;
mod handle;

pub use builder::*;
pub use handle::*;
