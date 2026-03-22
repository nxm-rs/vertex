//! Bootnode - minimal Swarm node with topology protocols only.
//!
//! A [`BootNode`] participates in peer discovery via handshake, hive, and pingpong
//! but does not run client protocols (pricing, retrieval, pushsync, settlement).
//!
//! Use this for dedicated bootnode servers that help new nodes join the network.

use std::ops::{Deref, DerefMut};

use eyre::Result;
use futures::StreamExt;
use libp2p::{identity::PublicKey, swarm::NetworkBehaviour, swarm::SwarmEvent};
use tracing::info;
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig};
use vertex_swarm_net_identify as identify;
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviour, TopologyConfig, TopologyEvent,
};
use vertex_tasks::GracefulShutdown;

use super::base::{BaseNode, BaseBehaviour};
use super::builder::BuiltInfrastructure;

/// Network behaviour for a bootnode (topology only, no client protocols).
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BootnodeEvent")]
pub struct BootnodeBehaviour<I: SwarmIdentity + Clone> {
    pub identify: identify::Behaviour,
    pub topology: TopologyBehaviour<I>,
}

impl<I: SwarmIdentity + Clone> BaseBehaviour<I> for BootnodeBehaviour<I> {
    fn topology(&self) -> &TopologyBehaviour<I> {
        &self.topology
    }

    fn topology_mut(&mut self) -> &mut TopologyBehaviour<I> {
        &mut self.topology
    }

    fn identify_mut(&mut self) -> &mut identify::Behaviour {
        &mut self.identify
    }
}

impl<I: SwarmIdentity + Clone> BootnodeBehaviour<I> {
    /// Create behaviour from pre-built topology (used with libp2p SwarmBuilder).
    pub fn from_parts(local_public_key: PublicKey, topology: TopologyBehaviour<I>) -> Self {
        let agent_versions = topology.agent_versions();
        Self {
            // Send listen addresses (even private IPs) so bee's peerstore has entries.
            // waitPeerAddrs() returns immediately if len(addrs) > 0, regardless of
            // whether addresses match or are reachable. The handshake protocol uses
            // RemoteMultiaddr directly. Private IPs in gossip are harmless.
            identify: identify::Behaviour::new(
                identify::Config::new(local_public_key),
                agent_versions,
            ),
            topology,
        }
    }
}

/// Events from the bootnode behaviour.
pub enum BootnodeEvent {
    Identify(Box<identify::Event>),
    Topology(()),
}

impl From<identify::Event> for BootnodeEvent {
    fn from(event: identify::Event) -> Self {
        BootnodeEvent::Identify(Box::new(event))
    }
}

impl From<()> for BootnodeEvent {
    fn from(_: ()) -> Self {
        BootnodeEvent::Topology(())
    }
}

/// A minimal Swarm node with only topology protocols.
///
/// Unlike [`ClientNode`](super::ClientNode), this excludes all client protocols
/// (pricing, retrieval, pushsync, settlement). Bootnodes only participate in
/// peer discovery via handshake, hive, and pingpong.
pub struct BootNode<I: SwarmIdentity + Clone> {
    base: BaseNode<I, BootnodeBehaviour<I>>,
    listen_addrs: Vec<libp2p::Multiaddr>,
}

impl<I: SwarmIdentity + Clone> Deref for BootNode<I> {
    type Target = BaseNode<I, BootnodeBehaviour<I>>;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl<I: SwarmIdentity + Clone> DerefMut for BootNode<I> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl<I: SwarmIdentity + Clone> BootNode<I> {
    pub fn builder(identity: I) -> BootNodeBuilder<I> {
        BootNodeBuilder::new(identity)
    }

    /// Start listening and run the event loop with graceful shutdown support.
    pub async fn start_and_run(mut self, shutdown: GracefulShutdown) -> Result<()> {
        let addrs = std::mem::take(&mut self.listen_addrs);
        self.base.start_listening(&addrs)?;
        self.run(shutdown).await
    }

    /// Run the event loop with graceful shutdown support.
    ///
    /// When the shutdown signal fires, the node will complete its current work
    /// and exit gracefully.
    pub async fn run(mut self, shutdown: GracefulShutdown) -> Result<()> {
        info!("Starting bootnode event loop");

        let mut topo_events = self.base.swarm.behaviour().topology.subscribe();
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    info!("Bootnode shutdown signal received");
                    self.base.save_peers();
                    drop(guard);
                    break;
                }
                event = self.base.swarm.next() => {
                    if let Some(event) = event {
                        self.handle_swarm_event(event);
                    }
                }
                result = topo_events.recv() => {
                    if let Ok(event) = result {
                        self.handle_topology_event(event);
                    }
                }
            }
        }

        info!("Bootnode shutdown complete");
        Ok(())
    }

    fn handle_topology_event(&mut self, _event: TopologyEvent) {
        // Topology events (PeerReady, etc.) don't require bootnode-level handling.
    }

    fn handle_swarm_event(&mut self, event: SwarmEvent<BootnodeEvent>) {
        if let Some(SwarmEvent::Behaviour(behaviour_event)) =
            self.base.handle_swarm_event_common(event)
        {
            self.handle_behaviour_event(behaviour_event);
        }
    }

    fn handle_behaviour_event(&mut self, event: BootnodeEvent) {
        match event {
            BootnodeEvent::Identify(boxed_event) => {
                self.base.handle_identify_event(*boxed_event);
            }
            BootnodeEvent::Topology(_) => {}
        }
    }
}

/// Builder for BootNode.
pub struct BootNodeBuilder<I: SwarmIdentity + Clone> {
    identity: I,
    infra: Option<BuiltInfrastructure<I>>,
    kademlia_config: Option<KademliaConfig>,
}

impl<I: SwarmIdentity + Clone> BootNodeBuilder<I> {
    pub fn new(identity: I) -> Self {
        Self {
            identity,
            infra: None,
            kademlia_config: None,
        }
    }

    pub fn with_infrastructure(mut self, infra: BuiltInfrastructure<I>) -> Self {
        self.infra = Some(infra);
        self
    }

    pub fn with_kademlia_config(mut self, kademlia_config: KademliaConfig) -> Self {
        self.kademlia_config = Some(kademlia_config);
        self
    }
}

impl<I: SwarmIdentity + Clone> BootNodeBuilder<I> {
    pub async fn build<C>(
        self,
        network_config: &C,
        peer_store: Option<
            std::sync::Arc<
                dyn vertex_net_peer_store::NetPeerStore<vertex_swarm_peer_manager::StoredPeer>,
            >,
        >,
        score_store: Option<
            std::sync::Arc<
                dyn vertex_swarm_api::SwarmScoreStore<
                        Value = vertex_swarm_peer_score::PeerScore,
                        Error = vertex_net_peer_store::error::StoreError,
                    >,
            >,
        >,
    ) -> Result<BootNode<I>>
    where
        I: vertex_swarm_spec::HasSpec,
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        info!("Initializing bootnode P2P network...");

        let infra = match self.infra {
            Some(infra) => infra,
            None => {
                let topology_config =
                    TopologyConfig::new().with_kademlia(self.kademlia_config.unwrap_or_default());
                BuiltInfrastructure::from_config(
                    self.identity,
                    network_config,
                    topology_config,
                    peer_store,
                    score_store,
                )?
            }
        };

        let (base, listen_addrs) = super::builder::build_base_node(
            infra,
            network_config,
            "Bootnode",
            BootnodeBehaviour::from_parts,
        )
        .await?;

        // Set local PeerId for address advertisement in handshakes
        base.swarm
            .behaviour()
            .topology
            .set_local_peer_id(*base.swarm.local_peer_id());

        Ok(BootNode { base, listen_addrs })
    }
}
