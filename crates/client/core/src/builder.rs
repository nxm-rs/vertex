//! Swarm builder infrastructure following the reth ComponentsBuilder pattern.
//!
//! Provides:
//! - [`SwarmBuilderContext`] - Runtime state for component builders
//! - Component builder traits ([`TopologyBuilder`], [`AccountingBuilder`])
//! - [`SwarmComponentsBuilder`] - Combines builders into a complete set

use std::sync::Arc;

use vertex_bandwidth_core::{Accounting, AccountingConfig, FixedPricer, Pricer};
use vertex_client_kademlia::{KademliaConfig, KademliaTopology};
use vertex_swarm_api::{
    AvailabilityAccounting, LightTypes, NetworkConfig, NoAvailabilityIncentives, Topology,
};
use vertex_tasks::TaskExecutor;

// ============================================================================
// Builder Context
// ============================================================================

/// Runtime context passed to component builders.
///
/// Contains everything needed to construct Swarm components:
/// - Identity and spec
/// - Network configuration
/// - Task executor for spawning
pub struct SwarmBuilderContext<'cfg, Types: LightTypes, Cfg: NetworkConfig> {
    /// The node's cryptographic identity.
    pub identity: Arc<Types::Identity>,

    /// Network configuration.
    pub config: &'cfg Cfg,

    /// Task executor for spawning background tasks.
    pub executor: TaskExecutor,
}

impl<'cfg, Types: LightTypes, Cfg: NetworkConfig> SwarmBuilderContext<'cfg, Types, Cfg> {
    /// Create a new builder context.
    pub fn new(identity: Arc<Types::Identity>, config: &'cfg Cfg, executor: TaskExecutor) -> Self {
        Self {
            identity,
            config,
            executor,
        }
    }
}

// ============================================================================
// Component Builder Traits
// ============================================================================

/// Builds the topology component.
pub trait TopologyBuilder<Types: LightTypes, Cfg: NetworkConfig>: Send + Sync + 'static {
    /// The topology type produced (may be Arc-wrapped).
    type Topology: Topology + Send + Sync + 'static;

    /// Build the topology given the context.
    fn build_topology(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Topology;
}

/// Builds the accounting component.
pub trait AccountingBuilder<Types: LightTypes, Cfg: NetworkConfig>: Send + Sync + 'static {
    /// The accounting type produced.
    type Accounting: AvailabilityAccounting + Send + Sync + 'static;

    /// Build the accounting given the context.
    fn build_accounting(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting;
}

/// Builds the pricer component.
pub trait PricerBuilder<Types: LightTypes, Cfg: NetworkConfig>: Send + Sync + 'static {
    /// The pricer type produced.
    type Pricer: Pricer + Clone + Send + Sync + 'static;

    /// Build the pricer given the context.
    fn build_pricer(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Pricer;
}

// ============================================================================
// Default Builders
// ============================================================================

/// Default Kademlia topology builder.
///
/// Produces `Arc<KademliaTopology<I>>` which implements `Topology`.
#[derive(Debug, Clone, Default)]
pub struct KademliaTopologyBuilder {
    config: KademliaConfig,
}

impl KademliaTopologyBuilder {
    /// Create with custom config.
    pub fn with_config(config: KademliaConfig) -> Self {
        Self { config }
    }
}

impl<Types, Cfg> TopologyBuilder<Types, Cfg> for KademliaTopologyBuilder
where
    Types: LightTypes,
    Types::Identity: Clone + Send + Sync + 'static,
    Cfg: NetworkConfig,
{
    type Topology = Arc<KademliaTopology<Types::Identity>>;

    fn build_topology(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Topology {
        let identity = (*ctx.identity).clone();
        let topology = KademliaTopology::new(identity, self.config);

        // Spawn the manage loop
        let _handle = topology.clone().spawn_manage_loop(&ctx.executor);

        topology
    }
}

/// Default bandwidth accounting builder.
///
/// Produces `Arc<Accounting>` which implements `AvailabilityAccounting`.
#[derive(Debug, Clone, Default)]
pub struct BandwidthAccountingBuilder {
    config: AccountingConfig,
}

impl BandwidthAccountingBuilder {
    /// Create with custom config.
    pub fn with_config(config: AccountingConfig) -> Self {
        Self { config }
    }
}

impl<Types: LightTypes, Cfg: NetworkConfig> AccountingBuilder<Types, Cfg>
    for BandwidthAccountingBuilder
{
    type Accounting = Arc<Accounting>;

    fn build_accounting(self, _ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting {
        Arc::new(Accounting::new(self.config))
    }
}

/// No-op accounting builder (for nodes without availability incentives).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAccountingBuilder;

impl<Types: LightTypes, Cfg: NetworkConfig> AccountingBuilder<Types, Cfg> for NoAccountingBuilder {
    type Accounting = NoAvailabilityIncentives;

    fn build_accounting(self, _ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting {
        NoAvailabilityIncentives
    }
}

/// Default fixed pricer builder.
#[derive(Debug, Clone, Default)]
pub struct FixedPricerBuilder {
    base_price: Option<u64>,
}

impl FixedPricerBuilder {
    /// Create with custom base price.
    pub fn with_base_price(base_price: u64) -> Self {
        Self {
            base_price: Some(base_price),
        }
    }
}

impl<Types: LightTypes, Cfg: NetworkConfig> PricerBuilder<Types, Cfg> for FixedPricerBuilder {
    type Pricer = FixedPricer;

    fn build_pricer(self, _ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Pricer {
        match self.base_price {
            Some(price) => FixedPricer::new(price),
            None => FixedPricer::default(),
        }
    }
}

// ============================================================================
// Swarm Components Builder
// ============================================================================

/// Combines individual component builders into a complete set.
///
/// Use the builder pattern to configure each component:
///
/// ```ignore
/// let components = SwarmComponentsBuilder::default()
///     .topology(KademliaTopologyBuilder::default())
///     .accounting(BandwidthAccountingBuilder::default())
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
        Types: LightTypes,
        Cfg: NetworkConfig,
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
pub struct BuiltSwarmComponents<T, A, P> {
    /// The topology.
    pub topology: T,
    /// The accounting.
    pub accounting: A,
    /// The pricer.
    pub pricer: P,
}

// ============================================================================
// Convenient Type Aliases
// ============================================================================

/// Default components builder with Kademlia topology, bandwidth accounting, fixed pricer.
pub type DefaultComponentsBuilder =
    SwarmComponentsBuilder<KademliaTopologyBuilder, BandwidthAccountingBuilder, FixedPricerBuilder>;

impl DefaultComponentsBuilder {
    /// Create a default components builder with standard implementations.
    pub fn with_defaults() -> Self {
        SwarmComponentsBuilder::new()
            .topology(KademliaTopologyBuilder::default())
            .accounting(BandwidthAccountingBuilder::default())
            .pricer(FixedPricerBuilder::default())
    }
}
