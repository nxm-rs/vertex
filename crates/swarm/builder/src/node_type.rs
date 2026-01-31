//! Node type markers for generic SwarmNodeBuilder.
//!
//! These marker types allow `SwarmNodeBuilder<N>` to be parameterized
//! by node type, enabling type-safe defaults and node-specific behavior.

use crate::components::{
    DefaultAccountingBuilder, FixedPricerBuilder, KademliaTopologyBuilder, NoAccountingBuilder,
    NoPricerBuilder,
};

/// Marker for client nodes (read + write, no storage).
#[derive(Debug, Clone, Copy, Default)]
pub struct Client;

/// Marker for storer nodes (storage + staking).
#[derive(Debug, Clone, Copy, Default)]
pub struct Storer;

/// Marker for bootnodes (topology only, no pricing).
#[derive(Debug, Clone, Copy, Default)]
pub struct Bootnode;

/// Default component builders for each node type.
pub trait NodeTypeDefaults: Send + Sync + 'static {
    /// Human-readable name for this node type.
    const NAME: &'static str;

    /// Default topology builder.
    type DefaultTopology: Default + Send + Sync + 'static;

    /// Default accounting builder.
    type DefaultAccounting: Default + Send + Sync + 'static;

    /// Default pricer builder.
    type DefaultPricer: Default + Send + Sync + 'static;
}

impl NodeTypeDefaults for Client {
    const NAME: &'static str = "Client";
    type DefaultTopology = KademliaTopologyBuilder;
    type DefaultAccounting = DefaultAccountingBuilder;
    type DefaultPricer = FixedPricerBuilder;
}

impl NodeTypeDefaults for Storer {
    const NAME: &'static str = "Storer";
    type DefaultTopology = KademliaTopologyBuilder;
    type DefaultAccounting = DefaultAccountingBuilder;
    type DefaultPricer = FixedPricerBuilder;
}

impl NodeTypeDefaults for Bootnode {
    const NAME: &'static str = "Bootnode";
    type DefaultTopology = KademliaTopologyBuilder;
    type DefaultAccounting = NoAccountingBuilder;
    type DefaultPricer = NoPricerBuilder;
}
