//! Shared builder infrastructure for node types.

use std::time::Duration;

use eyre::{Result, WrapErr};
use libp2p::{Multiaddr, SwarmBuilder, identity::PublicKey, noise, swarm::NetworkBehaviour, tcp, yamux};
use tracing::{info, warn};
use vertex_net_peer_store::NetPeerStore;
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig, SwarmTopologyCommands};
use vertex_net_peer_store::StoreError;
use vertex_swarm_api::SwarmScoreStore;
use vertex_swarm_peer_manager::StoredPeer;
use vertex_swarm_peer_score::PeerScore;
use vertex_swarm_spec::HasSpec;
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviour, TopologyConfig, TopologyHandle,
};

use super::base::BaseNode;
use super::error::NodeBuildError;

use crate::BootnodeProvider;

type PeerStore = std::sync::Arc<dyn NetPeerStore<StoredPeer>>;
type PeerScoreStore = std::sync::Arc<dyn SwarmScoreStore<Score = PeerScore, Error = StoreError>>;

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
        peer_store: Option<PeerStore>,
        score_store: Option<PeerScoreStore>,
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
            peer_store,
            score_store,
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

/// Build a libp2p Swarm and BaseNode from infrastructure and a behaviour factory.
///
/// Handles the common SwarmBuilder pipeline, peer ID logging, and bootnode
/// connection that all node types share.
///
/// The `behaviour_fn` receives the libp2p public key and the topology behaviour,
/// and must return both the composed NetworkBehaviour and a reference to its
/// topology so we can call `set_local_peer_id`.
pub(crate) async fn build_base_node<I, B, C, F>(
    mut infra: BuiltInfrastructure<I>,
    network_config: &C,
    node_type_name: &str,
    behaviour_fn: F,
) -> Result<BaseNode<I, B>>
where
    I: SwarmIdentity + Clone,
    B: NetworkBehaviour,
    C: SwarmNetworkConfig,
    F: FnOnce(PublicKey, TopologyBehaviour<I>) -> B,
{
    let topology_behaviour = infra
        .take_behaviour()
        .ok_or(NodeBuildError::TopologyBehaviourTaken)?;
    let idle_timeout = network_config.idle_timeout();
    let listen_addrs = network_config.listen_addrs().to_vec();

    let topology_cell = std::sync::Mutex::new(Some(topology_behaviour));

    let swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_dns()?
        .with_behaviour(|keypair| {
            let topology = topology_cell
                .lock()
                .map_err(|_| NodeBuildError::TopologyCellPoisoned)?
                .take()
                .ok_or(NodeBuildError::TopologyBehaviourTaken)?;
            Ok(behaviour_fn(keypair.public().clone(), topology))
        })?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(idle_timeout))
        .build();

    let local_peer_id = *swarm.local_peer_id();
    info!(%local_peer_id, "{} peer ID", node_type_name);
    info!(overlay = %infra.identity.overlay_address(), "Overlay address");

    if infra.topology_handle.connect_bootnodes().await.is_err() {
        warn!("Failed to send connect_bootnodes command");
    }

    Ok(BaseNode {
        swarm,
        identity: infra.identity,
        listen_addrs,
        topology_handle: infra.topology_handle,
    })
}
