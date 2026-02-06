//! Topology builder implementation for Kademlia routing.

use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig, SwarmSpec};

use crate::behaviour::{TopologyBehaviour, TopologyBehaviourConfig};
use crate::error::TopologyError;
use crate::handle::TopologyHandle;
use crate::handler::TopologyConfig;
use crate::routing::KademliaConfig;

/// Builds topology behaviour and handle from routing configuration.
pub trait SwarmTopologyBuilder<I: SwarmIdentity>: Clone + Send + Sync + 'static {
    /// Error type for build failures.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Build topology behaviour and handle.
    fn build<N>(
        &self,
        identity: I,
        config: &N,
    ) -> Result<(TopologyBehaviour<I>, TopologyHandle<I>), Self::Error>
    where
        N: SwarmNetworkConfig + SwarmPeerConfig;
}

impl<I: SwarmIdentity + Clone> SwarmTopologyBuilder<I> for KademliaConfig {
    type Error = TopologyError;

    fn build<N>(
        &self,
        identity: I,
        config: &N,
    ) -> Result<(TopologyBehaviour<I>, TopologyHandle<I>), Self::Error>
    where
        N: SwarmNetworkConfig + SwarmPeerConfig,
    {
        let bootnodes = if config.bootnodes().is_empty() {
            resolve_spec_bootnodes(identity.spec())
        } else {
            config.bootnodes().to_vec()
        };

        let behaviour_config = TopologyBehaviourConfig::new()
            .with_kademlia(self.clone())
            .with_nat_auto(config.nat_auto_enabled());

        let config_with_bootnodes = ConfigWithBootnodes {
            inner: config,
            bootnodes,
            kademlia: self.clone(),
        };

        TopologyBehaviour::new(
            identity,
            TopologyConfig::default(),
            behaviour_config,
            &config_with_bootnodes,
        )
    }
}

fn resolve_spec_bootnodes<S: SwarmSpec>(spec: &S) -> Vec<libp2p::Multiaddr> {
    spec.bootnodes()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| s.parse().ok())
        .collect()
}

struct ConfigWithBootnodes<'a, C> {
    inner: &'a C,
    bootnodes: Vec<libp2p::Multiaddr>,
    kademlia: KademliaConfig,
}

impl<C: SwarmNetworkConfig> SwarmNetworkConfig for ConfigWithBootnodes<'_, C> {
    fn listen_addrs(&self) -> &[libp2p::Multiaddr] {
        self.inner.listen_addrs()
    }

    fn bootnodes(&self) -> &[libp2p::Multiaddr] {
        &self.bootnodes
    }

    fn trusted_peers(&self) -> &[libp2p::Multiaddr] {
        self.inner.trusted_peers()
    }

    fn discovery_enabled(&self) -> bool {
        self.inner.discovery_enabled()
    }

    fn max_peers(&self) -> usize {
        self.inner.max_peers()
    }

    fn idle_timeout(&self) -> std::time::Duration {
        self.inner.idle_timeout()
    }

    fn nat_addrs(&self) -> &[libp2p::Multiaddr] {
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

impl<C> SwarmRoutingConfig for ConfigWithBootnodes<'_, C> {
    type Routing = KademliaConfig;

    fn routing(&self) -> &Self::Routing {
        &self.kademlia
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use vertex_swarm_api::DefaultPeerConfig;
    use vertex_swarm_identity::Identity;
    use vertex_swarm_primitives::SwarmNodeType;
    use vertex_swarm_spec::init_testnet;

    struct TestConfig {
        listen_addrs: Vec<libp2p::Multiaddr>,
        peer: DefaultPeerConfig,
    }

    impl Default for TestConfig {
        fn default() -> Self {
            Self {
                listen_addrs: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
                peer: DefaultPeerConfig::default(),
            }
        }
    }

    impl SwarmNetworkConfig for TestConfig {
        fn listen_addrs(&self) -> &[libp2p::Multiaddr] {
            &self.listen_addrs
        }

        fn bootnodes(&self) -> &[libp2p::Multiaddr] {
            &[]
        }

        fn discovery_enabled(&self) -> bool {
            true
        }

        fn max_peers(&self) -> usize {
            50
        }

        fn idle_timeout(&self) -> Duration {
            Duration::from_secs(60)
        }
    }

    impl SwarmPeerConfig for TestConfig {
        type Peers = DefaultPeerConfig;

        fn peers(&self) -> &Self::Peers {
            &self.peer
        }
    }

    #[tokio::test]
    async fn test_kademlia_builder() {
        let spec = init_testnet();
        let identity = Arc::new(Identity::random(spec, SwarmNodeType::Storer));
        let config = TestConfig::default();

        let kademlia_config = KademliaConfig::default();
        let result = kademlia_config.build(identity, &config);

        assert!(result.is_ok());
    }
}
