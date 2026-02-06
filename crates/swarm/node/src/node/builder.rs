//! Shared builder infrastructure for node types.

use std::time::Duration;

use eyre::{Result, WrapErr};
use libp2p::Multiaddr;
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig};
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviourComponents, TopologyHandle, TopologyService,
    TopologyServiceConfig,
};

use crate::BootnodeProvider;

/// Options for building topology infrastructure.
#[derive(Debug, Clone, Default)]
pub struct TopologyBuildOptions {
    /// Kademlia configuration override (uses defaults if None).
    pub kademlia_config: Option<KademliaConfig>,
}

impl TopologyBuildOptions {
    /// Create with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set Kademlia configuration.
    pub fn with_kademlia(mut self, config: KademliaConfig) -> Self {
        self.kademlia_config = Some(config);
        self
    }
}

/// Pre-built infrastructure components ready for swarm assembly.
pub struct BuiltInfrastructure<I: SwarmIdentity> {
    pub identity: I,
    /// Unified topology service owning routing, peer state, and address management.
    pub topology_service: TopologyService<I>,
    /// Unified topology handle for queries (wraps kademlia + peer_manager).
    pub topology_handle: TopologyHandle<I>,
    /// Components for TopologyBehaviour construction. Use .take() to extract.
    pub behaviour_components: Option<TopologyBehaviourComponents<I>>,
    pub listen_addrs: Vec<Multiaddr>,
    pub idle_timeout: Duration,
}

impl<I: SwarmIdentity> BuiltInfrastructure<I> {
    /// Build infrastructure from network configuration.
    ///
    /// Identity must be `Clone` (typically `Arc<Identity>`) to share with topology.
    /// If no bootnodes are provided in config, falls back to spec-defined defaults.
    pub fn from_config<C>(
        identity: I,
        network_config: &C,
        options: TopologyBuildOptions,
    ) -> Result<Self>
    where
        I: Clone,
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        // Determine bootnodes: use config or fall back to spec defaults
        let bootnodes = if network_config.bootnodes().is_empty() {
            BootnodeProvider::bootnodes(identity.spec())
        } else {
            network_config.bootnodes().to_vec()
        };

        // Build Kademlia config: use options override or config routing
        let kademlia_config = options
            .kademlia_config
            .unwrap_or_else(|| network_config.routing().clone());

        // Build topology-specific config
        let topology_config = TopologyServiceConfig::new().with_kademlia(kademlia_config);

        // Create a config adapter that provides the resolved bootnodes
        let config_with_bootnodes = ConfigWithBootnodes {
            inner: network_config,
            bootnodes,
        };

        let (topology_service, topology_handle, behaviour_components) =
            TopologyService::new(identity.clone(), &config_with_bootnodes, topology_config)
                .wrap_err("failed to create topology service")?;

        // Get listen addrs from topology service
        let listen_addrs = topology_service.listen_addrs().to_vec();

        Ok(Self {
            identity,
            topology_service,
            topology_handle,
            behaviour_components: Some(behaviour_components),
            listen_addrs,
            idle_timeout: network_config.idle_timeout(),
        })
    }

}

/// Config adapter that provides resolved bootnodes (from spec defaults if needed).
struct ConfigWithBootnodes<'a, C> {
    inner: &'a C,
    bootnodes: Vec<Multiaddr>,
}

impl<C: SwarmNetworkConfig> SwarmNetworkConfig for ConfigWithBootnodes<'_, C> {
    fn listen_addrs(&self) -> &[Multiaddr] {
        self.inner.listen_addrs()
    }

    fn bootnodes(&self) -> &[Multiaddr] {
        &self.bootnodes
    }

    fn trusted_peers(&self) -> &[Multiaddr] {
        self.inner.trusted_peers()
    }

    fn discovery_enabled(&self) -> bool {
        self.inner.discovery_enabled()
    }

    fn max_peers(&self) -> usize {
        self.inner.max_peers()
    }

    fn idle_timeout(&self) -> Duration {
        self.inner.idle_timeout()
    }

    fn nat_addrs(&self) -> &[Multiaddr] {
        self.inner.nat_addrs()
    }

    fn nat_auto_enabled(&self) -> bool {
        self.inner.nat_auto_enabled()
    }
}

impl<C: SwarmPeerConfig> SwarmPeerConfig for ConfigWithBootnodes<'_, C> {
    type Peers = C::Peers;

    fn peers(&self) -> &Self::Peers {
        self.inner.peers()
    }
}

impl<C: SwarmRoutingConfig> SwarmRoutingConfig for ConfigWithBootnodes<'_, C> {
    type Routing = C::Routing;

    fn routing(&self) -> &Self::Routing {
        self.inner.routing()
    }
}
