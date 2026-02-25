//! Shared builder infrastructure for node types.

use std::time::Duration;

use eyre::{Result, WrapErr};
use libp2p::Multiaddr;
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig};
use vertex_swarm_spec::HasSpec;
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviour, TopologyConfig, TopologyHandle,
};

use crate::BootnodeProvider;

/// Pre-built infrastructure components ready for swarm assembly.
pub struct BuiltInfrastructure<I: SwarmIdentity + Clone> {
    pub(crate) identity: I,
    pub(crate) topology_behaviour: Option<TopologyBehaviour<I>>,
    pub(crate) topology_handle: TopologyHandle<I>,
}

impl<I: SwarmIdentity + Clone> BuiltInfrastructure<I> {
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
        topology_config: TopologyConfig,
    ) -> Result<Self>
    where
        I: HasSpec,
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        let bootnodes = if network_config.bootnodes().is_empty() {
            BootnodeProvider::bootnodes(<I as SwarmIdentity>::spec(&identity))
        } else {
            network_config.bootnodes().to_vec()
        };

        let config_with_bootnodes = ConfigWithBootnodes {
            inner: network_config,
            bootnodes,
        };

        let (topology_behaviour, topology_handle) = TopologyBehaviour::new(
            identity.clone(),
            topology_config,
            &config_with_bootnodes,
        )
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
