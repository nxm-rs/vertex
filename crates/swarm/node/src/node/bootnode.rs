//! Bootnode - minimal Swarm node with topology protocols only.
//!
//! A [`BootNode`] participates in peer discovery via handshake, hive, and pingpong
//! but does not run client protocols (pricing, retrieval, pushsync, settlement).
//!
//! Use this for dedicated bootnode servers that help new nodes join the network.

use eyre::Result;
use futures::StreamExt;
use libp2p::{
    PeerId, SwarmBuilder, identity::PublicKey, noise, swarm::NetworkBehaviour,
    swarm::SwarmEvent, tcp, yamux,
};
use nectar_primitives::SwarmAddress;
use tracing::{debug, info, trace, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig};
use vertex_swarm_net_identify as identify;
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviour, TopologyCommand, TopologyConfig, TopologyEvent,
    TopologyHandle,
};
use vertex_tasks::GracefulShutdown;

use super::base::BaseNode;
use super::builder::BuiltInfrastructure;
use super::error::NodeBuildError;

/// Network behaviour for a bootnode (topology only, no client protocols).
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BootnodeEvent")]
pub struct BootnodeBehaviour<I: SwarmIdentity + Clone> {
    pub identify: identify::Behaviour,
    pub topology: TopologyBehaviour<I>,
}

impl<I: SwarmIdentity + Clone> BootnodeBehaviour<I> {
    /// Create behaviour from pre-built topology (used with libp2p SwarmBuilder).
    pub fn from_parts(local_public_key: PublicKey, topology: TopologyBehaviour<I>) -> Self {
        Self {
            // Send listen addresses (even private IPs) so bee's peerstore has entries.
            // waitPeerAddrs() returns immediately if len(addrs) > 0, regardless of
            // whether addresses match or are reachable. The handshake protocol uses
            // RemoteMultiaddr directly. Private IPs in gossip are harmless.
            identify: identify::Behaviour::new(identify::Config::new(local_public_key)),
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
}

impl<I: SwarmIdentity + Clone> BootNode<I> {
    pub fn builder(identity: I) -> BootNodeBuilder<I> {
        BootNodeBuilder::new(identity)
    }

    pub fn local_peer_id(&self) -> &PeerId {
        self.base.local_peer_id()
    }

    pub fn overlay_address(&self) -> SwarmAddress {
        self.base.overlay_address()
    }

    pub fn topology_handle(&self) -> &TopologyHandle<I> {
        self.base.topology_handle()
    }

    pub fn topology_command(&mut self, command: TopologyCommand) {
        self.base.swarm.behaviour_mut().topology.on_command(command);
    }

    pub fn start_listening(&mut self) -> Result<()> {
        self.base.start_listening()
    }

    /// Start listening and run the event loop with graceful shutdown support.
    pub async fn start_and_run(mut self, shutdown: GracefulShutdown) -> Result<()> {
        self.start_listening()?;
        self.run(shutdown).await
    }

    /// Run the event loop with graceful shutdown support.
    ///
    /// When the shutdown signal fires, the node will complete its current work
    /// and exit gracefully.
    pub async fn run(mut self, shutdown: GracefulShutdown) -> Result<()> {
        info!("Starting bootnode event loop");

        let mut topo_events = self.base.topology_handle.subscribe();
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    info!("Bootnode shutdown signal received");
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
                self.handle_identify_event(*boxed_event);
            }
            BootnodeEvent::Topology(_) => {}
        }
    }

    fn handle_identify_event(&mut self, event: identify::Event) {
        match event {
            identify::Event::Received { peer_id, info, .. } => {
                debug!(
                    %peer_id,
                    protocol_version = %info.protocol_version,
                    agent_version = %info.agent_version,
                    observed_addr = %info.observed_addr,
                    "Received identify info"
                );
                // Store agent version for diagnostics
                self.base
                    .topology_handle
                    .set_agent_version(&peer_id, info.agent_version.clone());

                // Record observed address for NAT discovery. This enables us to know
                // we're publicly reachable and connect to other public peers.
                if !info.observed_addr.is_empty() {
                    self.base
                        .swarm
                        .behaviour()
                        .topology
                        .on_observed_addr(&info.observed_addr);

                    // Push back our observed address to the peer. This bypasses go-libp2p's
                    // address filtering (which drops private IPs when connected from public IP)
                    // and ensures bee's peerstore has an entry for us, avoiding the 10s delay
                    // in waitPeerAddrs().
                    self.base
                        .swarm
                        .behaviour_mut()
                        .identify
                        .push_with_addresses(peer_id, vec![info.observed_addr]);
                }
            }
            identify::Event::Sent { peer_id, .. } => {
                trace!(%peer_id, "Sent identify info");
            }
            identify::Event::Pushed { peer_id, .. } => {
                debug!(%peer_id, "Pushed identify info");
            }
            identify::Event::Error { peer_id, error, .. } => {
                warn!(%peer_id, %error, "Identify error");
            }
        }
    }

    pub fn connected_peers(&self) -> usize {
        self.base.connected_peers()
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
    pub async fn build<C>(self, network_config: &C) -> Result<BootNode<I>>
    where
        I: vertex_swarm_spec::HasSpec,
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        info!("Initializing bootnode P2P network...");

        let mut infra = match self.infra {
            Some(infra) => infra,
            None => {
                let topology_config = TopologyConfig::new()
                    .with_kademlia(self.kademlia_config.unwrap_or_default());
                BuiltInfrastructure::from_config(self.identity, network_config, topology_config)?
            }
        };

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
                Ok(BootnodeBehaviour::from_parts(keypair.public().clone(), topology))
            })?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(idle_timeout))
            .build();

        let local_peer_id = *swarm.local_peer_id();
        info!(%local_peer_id, "Bootnode peer ID");
        info!(overlay = %infra.identity.overlay_address(), "Overlay address");

        // Set local PeerId for address advertisement in handshakes
        swarm.behaviour().topology.set_local_peer_id(local_peer_id);

        if infra.topology_handle.connect_bootnodes().await.is_err() {
            warn!("Failed to send connect_bootnodes command");
        }

        let base = BaseNode {
            swarm,
            identity: infra.identity,
            listen_addrs,
            topology_handle: infra.topology_handle,
        };

        Ok(BootNode { base })
    }
}
