//! Shared builder infrastructure for node types.

use std::time::Duration;

use eyre::{Result, WrapErr};
use libp2p::Multiaddr;
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig};
use vertex_swarm_topology::{KademliaConfig, SwarmTopologyBuilder, TopologyBehaviour, TopologyHandle};

use crate::BootnodeProvider;

/// Options for building topology infrastructure.
#[derive(Debug, Clone, Default)]
pub struct TopologyBuildOptions {
    /// Kademlia configuration override (uses defaults if None).
    pub kademlia_config: Option<KademliaConfig>,
}

impl TopologyBuildOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_kademlia(mut self, config: KademliaConfig) -> Self {
        self.kademlia_config = Some(config);
        self
    }
}

/// Pre-built infrastructure components ready for swarm assembly.
pub struct BuiltInfrastructure<I: SwarmIdentity> {
    pub(crate) identity: I,
    pub(crate) topology_behaviour: Option<TopologyBehaviour<I>>,
    pub(crate) topology_handle: TopologyHandle<I>,
}

impl<I: SwarmIdentity> BuiltInfrastructure<I> {
    /// Get the identity.
    pub fn identity(&self) -> &I {
        &self.identity
    }

    /// Get the topology handle.
    pub fn topology_handle(&self) -> &TopologyHandle<I> {
        &self.topology_handle
    }

    /// Take the topology behaviour (can only be called once).
    pub fn take_behaviour(&mut self) -> Option<TopologyBehaviour<I>> {
        self.topology_behaviour.take()
    }
}

impl<I: SwarmIdentity + Clone> BuiltInfrastructure<I> {
    /// Build infrastructure from network configuration.
    pub fn from_config<C>(
        identity: I,
        network_config: &C,
        options: TopologyBuildOptions,
    ) -> Result<Self>
    where
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        let bootnodes = if network_config.bootnodes().is_empty() {
            BootnodeProvider::bootnodes(identity.spec())
        } else {
            network_config.bootnodes().to_vec()
        };

        let kademlia_config = options
            .kademlia_config
            .unwrap_or_else(|| network_config.routing().clone());

        let config_with_bootnodes = ConfigWithBootnodes {
            inner: network_config,
            bootnodes: bootnodes.clone(),
        };

        let (topology_behaviour, topology_handle) = kademlia_config
            .build(identity.clone(), &config_with_bootnodes)
            .wrap_err("failed to create topology behaviour")?;

        Ok(Self {
            identity,
            topology_behaviour: Some(topology_behaviour),
            topology_handle,
        })
    }
}

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
