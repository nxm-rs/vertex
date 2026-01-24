//! Swarm component builder infrastructure.
//!
//! This crate provides the builder pattern for constructing Swarm components
//! (topology, accounting, pricer). Follows the reth `ComponentsBuilder` pattern
//! where each component has a dedicated builder trait.
//!
//! # Design
//!
//! Builders are separated from implementations to allow:
//! - Dependency injection (swap implementations without changing consumers)
//! - Testing with mock builders
//! - Configuration-driven component selection
//!
//! # Core Types
//!
//! - [`SwarmBuilderContext`] - Runtime context passed to all builders
//! - [`SwarmComponentsBuilder`] - Combines individual builders
//! - [`BuiltSwarmComponents`] - The constructed components
//!
//! # Builder Traits
//!
//! - [`TopologyBuilder`] - Builds peer topology (e.g., Kademlia)
//! - [`AccountingBuilder`] - Builds availability accounting
//! - [`PricerBuilder`] - Builds bandwidth pricing
//!
//! Builders return their associated types, deciding whether to wrap in `Arc`
//! for types requiring interior mutability.

mod accounting;
mod components;
mod context;
mod pricer;
mod topology;

pub use accounting::{AccountingBuilder, BandwidthAccountingBuilder, NoAccountingBuilder};
pub use components::{BuiltSwarmComponents, DefaultComponentsBuilder, SwarmComponentsBuilder};
pub use context::SwarmBuilderContext;
pub use pricer::{FixedPricerBuilder, PricerBuilder};
pub use topology::{KademliaTopologyBuilder, TopologyBuilder};
