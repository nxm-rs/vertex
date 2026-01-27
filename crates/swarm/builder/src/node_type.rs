//! Node type markers for generic SwarmNodeBuilder.
//!
//! These marker types allow `SwarmNodeBuilder<N>` to be parameterized
//! by node type, enabling type-safe defaults and node-specific behavior.

use crate::{
    BandwidthAccountingBuilder, FixedPricerBuilder, KademliaTopologyBuilder, NoAccountingBuilder,
};

/// Marker type for light nodes.
///
/// Light nodes can retrieve chunks from the network but don't store data.
#[derive(Debug, Clone, Copy, Default)]
pub struct Light;

/// Marker type for full nodes.
///
/// Full nodes store chunks and participate in the storage network.
#[derive(Debug, Clone, Copy, Default)]
pub struct Full;

/// Marker type for bootnode nodes.
///
/// Bootnodes only participate in topology (Kademlia/Hive), no retrieval or storage.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bootnode;

/// Marker type for publisher nodes.
///
/// Publisher nodes can retrieve and upload chunks but don't store long-term.
#[derive(Debug, Clone, Copy, Default)]
pub struct Publisher;

/// Marker type for staker nodes.
///
/// Staker nodes participate in storage incentives and stake verification.
#[derive(Debug, Clone, Copy, Default)]
pub struct Staker;

/// Trait defining default component builders for each node type.
///
/// Each node type marker implements this trait to specify which builders
/// are used by default when creating a `SwarmNodeBuilder<N>::new()`.
pub trait NodeTypeDefaults: Send + Sync + 'static {
    /// Human-readable name for this node type (e.g., "Light", "Full").
    const NAME: &'static str;

    /// Default topology builder for this node type.
    type DefaultTopology: Default + Send + Sync + 'static;

    /// Default accounting builder for this node type.
    type DefaultAccounting: Default + Send + Sync + 'static;

    /// Default pricer builder for this node type.
    type DefaultPricer: Default + Send + Sync + 'static;
}

impl NodeTypeDefaults for Light {
    const NAME: &'static str = "Light";
    type DefaultTopology = KademliaTopologyBuilder;
    type DefaultAccounting = BandwidthAccountingBuilder;
    type DefaultPricer = FixedPricerBuilder;
}

impl NodeTypeDefaults for Full {
    const NAME: &'static str = "Full";
    type DefaultTopology = KademliaTopologyBuilder;
    type DefaultAccounting = BandwidthAccountingBuilder;
    type DefaultPricer = FixedPricerBuilder;
}

impl NodeTypeDefaults for Bootnode {
    const NAME: &'static str = "Bootnode";
    type DefaultTopology = KademliaTopologyBuilder;
    type DefaultAccounting = NoAccountingBuilder;
    type DefaultPricer = FixedPricerBuilder;
}

impl NodeTypeDefaults for Publisher {
    const NAME: &'static str = "Publisher";
    type DefaultTopology = KademliaTopologyBuilder;
    type DefaultAccounting = BandwidthAccountingBuilder;
    type DefaultPricer = FixedPricerBuilder;
}

impl NodeTypeDefaults for Staker {
    const NAME: &'static str = "Staker";
    type DefaultTopology = KademliaTopologyBuilder;
    type DefaultAccounting = BandwidthAccountingBuilder;
    type DefaultPricer = FixedPricerBuilder;
}
