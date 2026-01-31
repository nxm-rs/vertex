//! Swarm components builder combining individual builders.

mod accounting;
mod pricer;
mod topology;

pub use accounting::{
    AccountingBuilder, CombinedAccountingBuilder, ModeBasedAccounting,
    ModeBasedAccountingBuilder, NoAccountingBuilder, PseudosettleAccountingBuilder,
    SwapAccountingBuilder,
};
// Re-export DefaultAccountingConfig from vertex-swarm-api for convenience
pub use vertex_swarm_api::DefaultAccountingConfig;
pub use pricer::{FixedPricerBuilder, NoPricerBuilder, PricerBuilder};
pub use topology::{KademliaTopologyBuilder, TopologyBuilder};

use vertex_swarm_api::{SwarmClientTypes, SwarmNetworkConfig};

use crate::SwarmBuilderContext;

/// Combines individual component builders into a complete set.
///
/// Use the builder pattern to configure each component:
///
/// ```ignore
/// let components = SwarmComponentsBuilder::default()
///     .topology(KademliaTopologyBuilder::default())
///     .accounting(PseudosettleAccountingBuilder::default())
///     .pricer(FixedPricerBuilder::default())
///     .build(&ctx);
/// ```
pub struct SwarmComponentsBuilder<TB, AB, PB> {
    topology_builder: TB,
    accounting_builder: AB,
    pricer_builder: PB,
}

impl Default for SwarmComponentsBuilder<(), (), ()> {
    fn default() -> Self {
        Self {
            topology_builder: (),
            accounting_builder: (),
            pricer_builder: (),
        }
    }
}

impl SwarmComponentsBuilder<(), (), ()> {
    /// Create a new empty components builder.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<TB, AB, PB> SwarmComponentsBuilder<TB, AB, PB> {
    /// Set the topology builder.
    pub fn topology<NewTB>(self, builder: NewTB) -> SwarmComponentsBuilder<NewTB, AB, PB> {
        SwarmComponentsBuilder {
            topology_builder: builder,
            accounting_builder: self.accounting_builder,
            pricer_builder: self.pricer_builder,
        }
    }

    /// Set the accounting builder.
    pub fn accounting<NewAB>(self, builder: NewAB) -> SwarmComponentsBuilder<TB, NewAB, PB> {
        SwarmComponentsBuilder {
            topology_builder: self.topology_builder,
            accounting_builder: builder,
            pricer_builder: self.pricer_builder,
        }
    }

    /// Set the pricer builder.
    pub fn pricer<NewPB>(self, builder: NewPB) -> SwarmComponentsBuilder<TB, AB, NewPB> {
        SwarmComponentsBuilder {
            topology_builder: self.topology_builder,
            accounting_builder: self.accounting_builder,
            pricer_builder: builder,
        }
    }

    /// Build all components using the context.
    pub fn build<Types, Cfg>(
        self,
        ctx: &SwarmBuilderContext<'_, Types, Cfg>,
    ) -> BuiltSwarmComponents<TB::Topology, AB::Accounting, PB::Pricer>
    where
        Types: SwarmClientTypes,
        Cfg: SwarmNetworkConfig,
        TB: TopologyBuilder<Types, Cfg>,
        AB: AccountingBuilder<Types, Cfg>,
        PB: PricerBuilder<Types, Cfg>,
    {
        BuiltSwarmComponents {
            topology: self.topology_builder.build_topology(ctx),
            accounting: self.accounting_builder.build_accounting(ctx),
            pricer: self.pricer_builder.build_pricer(ctx),
        }
    }
}

/// Built components ready for use.
///
/// The builders decide whether to wrap types in `Arc` for cheap cloning.
/// For example, `PseudosettleAccountingBuilder` returns `Arc<Accounting>`.
#[derive(Debug, Clone)]
pub struct BuiltSwarmComponents<T, A, P> {
    /// The topology.
    pub topology: T,
    /// The accounting.
    pub accounting: A,
    /// The pricer.
    pub pricer: P,
}

/// Default components builder with Kademlia topology, pseudosettle accounting, fixed pricer.
pub type DefaultComponentsBuilder = SwarmComponentsBuilder<
    KademliaTopologyBuilder,
    PseudosettleAccountingBuilder<DefaultAccountingConfig>,
    FixedPricerBuilder,
>;

impl DefaultComponentsBuilder {
    /// Create a default components builder with standard implementations.
    pub fn with_defaults() -> Self {
        SwarmComponentsBuilder::new()
            .topology(KademliaTopologyBuilder::default())
            .accounting(PseudosettleAccountingBuilder::default())
            .pricer(FixedPricerBuilder::default())
    }
}
