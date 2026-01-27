//! Swarm node builder infrastructure.
//!
//! This crate provides the builder pattern for launching Swarm nodes and
//! constructing Swarm components.
//!
//! # Node Building
//!
//! The primary entry point is `NodeBuilder` from `vertex-node-builder`, combined
//! with `SwarmNodeBuilder<N>` for protocol configuration:
//!
//! ```ignore
//! use vertex_node_builder::NodeBuilder;
//! use vertex_swarm_builder::{SwarmNodeBuilder, node_type};
//!
//! let handle = NodeBuilder::new()
//!     .with_context(&ctx, &args.infra)
//!     .with_protocol(SwarmNodeBuilder::<node_type::Light>::new(&ctx, &args.swarm))
//!     .launch()
//!     .await?;
//!
//! handle.wait_for_exit().await?;
//! ```
//!
//! # Component Building
//!
//! Lower-level component building is also available via:
//! - [`SwarmBuilderContext`] - Runtime context passed to all builders
//! - [`SwarmComponentsBuilder`] - Combines individual builders
//! - [`TopologyBuilder`], [`AccountingBuilder`], [`PricerBuilder`] - Individual component builders

mod components;
mod context;
mod error;
mod launch;
mod node;
pub mod node_type;
mod types;

// Node building
pub use error::SwarmNodeError;
pub use launch::{
    SwarmLaunchContext, create_and_save_signer, load_signer_from_keystore, resolve_password,
};
pub use node::{LightNodeBuildConfig, SwarmNodeBuilder};
pub use types::{ClientServiceRunner, DefaultLightTypes, DefaultNetworkConfig, SwarmNodeRunner};

// Component building
pub use components::{
    AccountingBuilder, BandwidthAccountingBuilder, BuiltSwarmComponents, DefaultComponentsBuilder,
    FixedPricerBuilder, KademliaTopologyBuilder, NoAccountingBuilder, PricerBuilder,
    SwarmComponentsBuilder, TopologyBuilder,
};
pub use context::SwarmBuilderContext;
