//! Bootnode network behaviour and node - topology only, no client protocols.

use std::sync::Arc;
use std::time::Duration;

use eyre::Result;
use futures::StreamExt;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{
    Multiaddr, PeerId, Swarm, SwarmBuilder, identify, identity::PublicKey, noise,
    swarm::SwarmEvent, tcp, yamux,
};
use nectar_primitives::SwarmAddress;
use tracing::{debug, info, warn};
use vertex_swarm_api::SwarmTopology;
use vertex_swarm_api::{SwarmIdentity, SwarmNodeTypes};
use vertex_swarm_kademlia::{KademliaConfig, KademliaTopology};
use vertex_swarm_peermanager::{
    AddressManager, DiscoverySender, InternalPeerManager, PeerManager, PeerStore,
    discovery_channel, run_peer_store_consumer,
};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_topology::{
    BehaviourConfig as TopologyBehaviourConfig, BootnodeConnector, SwarmTopologyBehaviour,
    TopologyCommand, TopologyEvent, is_dnsaddr,
};
use vertex_tasks::SpawnableTask;
use vertex_tasks::TaskExecutor;

use crate::BootnodeProvider;

/// Bootnode network behaviour with only topology protocols.
///
/// Unlike [`SwarmNodeBehaviour`](super::behaviour::SwarmNodeBehaviour), this excludes
/// client protocols (pricing, retrieval, pushsync, settlement).
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BootnodeEvent")]
pub struct BootnodeBehaviour<N: SwarmNodeTypes> {
    /// Identify protocol - exchange peer info.
    pub identify: identify::Behaviour,

    /// Topology behaviour - handshake, hive, pingpong only.
    pub topology: SwarmTopologyBehaviour<N>,
}

impl<N: SwarmNodeTypes> BootnodeBehaviour<N> {
    /// Create a new bootnode behaviour.
    pub fn new(local_public_key: PublicKey, identity: N::Identity) -> Self {
        Self {
            identify: identify::Behaviour::new(identify::Config::new(
                "/vertex/1.0.0".to_string(),
                local_public_key,
            )),
            topology: SwarmTopologyBehaviour::new(identity, TopologyBehaviourConfig::default()),
        }
    }
}

/// Events from the bootnode behaviour.
pub enum BootnodeEvent {
    /// Identify protocol event.
    Identify(identify::Event),
    /// Topology event (peer ready, disconnected, discovered).
    Topology(TopologyEvent),
}

impl From<identify::Event> for BootnodeEvent {
    fn from(event: identify::Event) -> Self {
        BootnodeEvent::Identify(event)
    }
}

impl From<TopologyEvent> for BootnodeEvent {
    fn from(event: TopologyEvent) -> Self {
        BootnodeEvent::Topology(event)
    }
}

/// A bootnode - minimal swarm node with only topology protocols.
///
/// Unlike [`SwarmNode`](super::SwarmNode), this excludes all client protocols
/// (pricing, retrieval, pushsync, settlement). Bootnodes only participate in
/// peer discovery via handshake, hive, and pingpong.
pub struct BootNode<N: SwarmNodeTypes> {
    swarm: Swarm<BootnodeBehaviour<N>>,
    identity: N::Identity,
    peer_manager: Arc<PeerManager>,
    address_manager: Option<Arc<AddressManager>>,
    kademlia: Arc<KademliaTopology<N::Identity>>,
    bootnode_connector: BootnodeConnector,
    listen_addrs: Vec<Multiaddr>,
    discovery_tx: DiscoverySender,
}

impl<N: SwarmNodeTypes> BootNode<N> {
    /// Create a builder for constructing a BootNode.
    pub fn builder(identity: N::Identity) -> BootNodeBuilder<N> {
        BootNodeBuilder::new(identity)
    }

    /// Get the local peer ID.
    pub fn local_peer_id(&self) -> &PeerId {
        self.swarm.local_peer_id()
    }

    /// Get the overlay address.
    pub fn overlay_address(&self) -> SwarmAddress {
        self.identity.overlay_address()
    }

    /// Get the swarm identity.
    pub fn identity(&self) -> &N::Identity {
        &self.identity
    }

    /// Get the peer manager.
    pub fn peer_manager(&self) -> &Arc<PeerManager> {
        &self.peer_manager
    }

    /// Get the Kademlia topology.
    pub fn kademlia_topology(&self) -> &Arc<KademliaTopology<N::Identity>> {
        &self.kademlia
    }

    /// Send a topology command.
    pub fn topology_command(&mut self, command: TopologyCommand) {
        self.swarm.behaviour_mut().topology.on_command(command);
    }

    /// Start listening on configured addresses.
    pub fn start_listening(&mut self) -> Result<()> {
        for addr in &self.listen_addrs {
            match self.swarm.listen_on(addr.clone()) {
                Ok(_) => info!(%addr, "Listening on address"),
                Err(e) => warn!(%addr, %e, "Failed to listen on address"),
            }
        }
        Ok(())
    }

    /// Connect to bootnodes.
    pub async fn connect_bootnodes(&mut self) -> Result<usize> {
        let bootnodes = self.bootnode_connector.shuffled_bootnodes();

        if bootnodes.is_empty() {
            warn!("No bootnodes configured");
            return Ok(0);
        }

        info!(count = bootnodes.len(), "Connecting to bootnodes...");

        let mut connected = 0;
        let min_connections = self.bootnode_connector.min_connections();

        for bootnode in bootnodes {
            if connected >= min_connections {
                info!(connected, "Reached minimum bootnode connections");
                break;
            }

            let is_dnsaddr = is_dnsaddr(&bootnode);
            info!(
                %bootnode,
                is_dnsaddr,
                "Dialing bootnode{}",
                if is_dnsaddr { " (dnsaddr will be resolved)" } else { "" }
            );

            match self.swarm.dial(bootnode.clone()) {
                Ok(_) => {
                    debug!(%bootnode, "Dial initiated");
                    connected += 1;
                }
                Err(e) => {
                    warn!(%bootnode, %e, "Failed to dial bootnode");
                }
            }
        }

        Ok(connected)
    }

    /// Start listening and run the event loop.
    async fn start_and_run(mut self) -> Result<()> {
        self.start_listening()?;
        self.connect_bootnodes().await?;
        self.run().await
    }

    /// Run the network event loop.
    pub async fn run(mut self) -> Result<()> {
        info!("Starting bootnode event loop");

        loop {
            if let Some(event) = self.swarm.next().await {
                self.handle_swarm_event(event);
            }
        }
    }

    fn handle_swarm_event(&mut self, event: SwarmEvent<BootnodeEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(%address, "New listen address");
                if let Some(mgr) = &self.address_manager {
                    mgr.on_new_listen_addr(address.clone());
                }
            }
            SwarmEvent::ExpiredListenAddr { address, .. } => {
                info!(%address, "Expired listen address");
                if let Some(mgr) = &self.address_manager {
                    mgr.on_expired_listen_addr(&address);
                }
            }
            SwarmEvent::ConnectionEstablished {
                peer_id,
                endpoint,
                num_established,
                ..
            } => {
                debug!(
                    %peer_id,
                    endpoint = %endpoint.get_remote_address(),
                    num_established,
                    "Connection established"
                );
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                cause,
                num_established,
                ..
            } => {
                info!(%peer_id, num_established, cause = ?cause, "Connection closed");
            }
            SwarmEvent::IncomingConnection {
                local_addr,
                send_back_addr,
                ..
            } => {
                debug!(%local_addr, %send_back_addr, "Incoming connection");
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                if let Some(peer_id) = peer_id {
                    warn!(%peer_id, %error, "Outgoing connection error");
                    if let Some(overlay) = self.peer_manager.on_peer_disconnected(&peer_id) {
                        self.kademlia.connection_failed(&overlay);
                    }
                } else {
                    warn!(%error, "Outgoing connection error (unknown peer)");
                }
            }
            SwarmEvent::Behaviour(event) => {
                self.handle_behaviour_event(event);
            }
            _ => {}
        }
    }

    fn handle_behaviour_event(&mut self, event: BootnodeEvent) {
        match event {
            BootnodeEvent::Identify(identify::Event::Received { peer_id, info, .. }) => {
                debug!(
                    %peer_id,
                    protocol_version = %info.protocol_version,
                    agent_version = %info.agent_version,
                    "Received identify info"
                );
            }
            BootnodeEvent::Identify(identify::Event::Sent { peer_id, .. }) => {
                debug!(%peer_id, "Sent identify info");
            }
            BootnodeEvent::Identify(identify::Event::Pushed { peer_id, .. }) => {
                debug!(%peer_id, "Pushed identify info");
            }
            BootnodeEvent::Identify(identify::Event::Error { peer_id, error, .. }) => {
                warn!(%peer_id, %error, "Identify error");
            }
            BootnodeEvent::Topology(event) => {
                self.handle_topology_event(event);
            }
        }
    }

    fn handle_topology_event(&mut self, event: TopologyEvent) {
        match event {
            TopologyEvent::PeerAuthenticated {
                peer_id,
                connection_id: _,
                info,
            } => {
                let overlay = OverlayAddress::new((*info.swarm_peer.overlay()).into());
                let is_full_node = info.full_node;

                debug!(%peer_id, %overlay, %is_full_node, "Peer authenticated");

                self.peer_manager
                    .on_peer_ready(peer_id, overlay, is_full_node);
                self.kademlia.connected(overlay);
            }
            TopologyEvent::PeerConnectionClosed { peer_id } => {
                if let Some(overlay) = self.peer_manager.on_peer_disconnected(&peer_id) {
                    debug!(%peer_id, %overlay, "Peer disconnected");
                    self.kademlia.disconnected(&overlay);
                }
            }
            TopologyEvent::HivePeersReceived { from, peers } => {
                debug!(%from, count = peers.len(), "Received peers via hive");

                let mut overlays = Vec::with_capacity(peers.len());
                let mut multiaddr_entries = Vec::with_capacity(peers.len());

                for peer in &peers {
                    let overlay = OverlayAddress::from(*peer.overlay());
                    overlays.push(overlay);
                    multiaddr_entries.push((overlay, peer.multiaddrs().to_vec()));
                }

                self.peer_manager.cache_multiaddrs_batch(multiaddr_entries);

                for peer in peers {
                    let _ = self.discovery_tx.send(peer);
                }

                self.kademlia.add_peers(&overlays);
                self.kademlia.evaluate_connections();
                self.dial_connection_candidates();
            }
            TopologyEvent::DialFailed { address, error } => {
                warn!(%address, %error, "Dial failed");
            }
            TopologyEvent::DepthChanged { new_depth } => {
                info!(%new_depth, "Network depth changed");
            }
        }
    }

    fn dial_connection_candidates(&mut self) {
        let candidates = self.kademlia.peers_to_connect();
        let dialable = self.peer_manager.filter_dialable_candidates(&candidates);

        for (overlay, multiaddrs) in dialable {
            let Some((addr, peer_id)) = multiaddrs.iter().find_map(|addr| {
                addr.iter().find_map(|p| {
                    if let libp2p::multiaddr::Protocol::P2p(id) = p {
                        Some((addr.clone(), id))
                    } else {
                        None
                    }
                })
            }) else {
                continue;
            };

            if self.swarm.is_connected(&peer_id) {
                continue;
            }

            debug!(%overlay, %addr, %peer_id, "Dialing discovered peer");

            if !self.peer_manager.start_connecting(overlay) {
                continue;
            }

            self.kademlia.start_connecting(overlay);

            if let Err(e) = self.swarm.dial(addr.clone()) {
                debug!(%overlay, %addr, %e, "Failed to dial");
                self.peer_manager.connection_failed(&overlay);
            }
        }
    }

    /// Get the number of connected peers.
    pub fn connected_peers(&self) -> usize {
        self.swarm.connected_peers().count()
    }
}

impl<N: SwarmNodeTypes> SpawnableTask for BootNode<N> {
    fn into_task(self) -> impl std::future::Future<Output = ()> + Send {
        async move {
            if let Err(e) = self.start_and_run().await {
                tracing::error!(error = %e, "BootNode error");
            }
        }
    }
}

/// Builder for BootNode.
pub struct BootNodeBuilder<N: SwarmNodeTypes> {
    identity: N::Identity,
    listen_addrs: Vec<Multiaddr>,
    bootnodes: Vec<Multiaddr>,
    idle_timeout: Duration,
    kademlia_config: KademliaConfig,
    peer_store: Option<Arc<dyn PeerStore>>,
    nat_addrs: Vec<Multiaddr>,
    nat_auto: bool,
}

impl<N: SwarmNodeTypes> BootNodeBuilder<N> {
    /// Create a new builder.
    pub fn new(identity: N::Identity) -> Self {
        Self {
            identity,
            listen_addrs: vec![
                "/ip4/0.0.0.0/tcp/1634".parse().unwrap(),
                "/ip6/::/tcp/1634".parse().unwrap(),
            ],
            bootnodes: vec![],
            idle_timeout: Duration::from_secs(30),
            kademlia_config: KademliaConfig::default(),
            peer_store: None,
            nat_addrs: vec![],
            nat_auto: false,
        }
    }

    /// Set network configuration.
    pub fn with_network_config(
        mut self,
        config: &impl vertex_swarm_api::SwarmNetworkConfig,
    ) -> Self {
        self.listen_addrs = config
            .listen_addrs()
            .into_iter()
            .filter_map(|s| s.parse().ok())
            .collect();

        let config_bootnodes: Vec<Multiaddr> = config
            .bootnodes()
            .into_iter()
            .filter_map(|s| s.parse().ok())
            .collect();

        self.bootnodes = if config_bootnodes.is_empty() {
            BootnodeProvider::bootnodes(self.identity.spec())
        } else {
            config_bootnodes
        };

        self.idle_timeout = config.idle_timeout();
        self.nat_addrs = config
            .nat_addrs()
            .into_iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        self.nat_auto = config.nat_auto_enabled();

        self
    }

    /// Set the peer store.
    pub fn with_peer_store(mut self, store: Arc<dyn PeerStore>) -> Self {
        self.peer_store = Some(store);
        self
    }

    /// Build the BootNode.
    pub async fn build(self) -> Result<BootNode<N>> {
        info!("Initializing bootnode P2P network...");

        let identity = self.identity;

        let address_manager = {
            let mgr = AddressManager::new(self.nat_addrs.clone(), self.nat_auto);
            if !self.nat_addrs.is_empty() {
                info!(count = self.nat_addrs.len(), "NAT addresses configured");
            }
            if self.nat_auto {
                info!("Auto NAT discovery enabled");
            }
            Some(Arc::new(mgr))
        };

        let identity_for_behaviour = identity.clone();
        let swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_dns()?
            .with_behaviour(|keypair| {
                Ok(BootnodeBehaviour::new(
                    keypair.public().clone(),
                    identity_for_behaviour.clone(),
                ))
            })?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(self.idle_timeout))
            .build();

        let local_peer_id = *swarm.local_peer_id();
        info!(%local_peer_id, "Bootnode peer ID");
        info!(overlay = %identity.overlay_address(), "Overlay address");

        let peer_manager = match self.peer_store {
            Some(store) => {
                let pm = PeerManager::with_store(store)
                    .map_err(|e| eyre::eyre!("failed to init peer manager: {}", e))?;
                info!(count = pm.stats().stored_peers, "loaded peers from store");
                Arc::new(pm)
            }
            None => Arc::new(PeerManager::new()),
        };

        let kademlia = KademliaTopology::new(identity.clone(), self.kademlia_config);

        let known_peers = peer_manager.known_dialable_peers();
        if !known_peers.is_empty() {
            info!(
                count = known_peers.len(),
                "seeding kademlia with stored peers"
            );
            kademlia.add_peers(&known_peers);
        }

        let executor = TaskExecutor::current();
        let _manage_handle = kademlia.clone().spawn_manage_loop(&executor);

        let (discovery_tx, discovery_rx) = discovery_channel();

        let pm_for_consumer = peer_manager.clone();
        executor.spawn(async move {
            run_peer_store_consumer(pm_for_consumer, discovery_rx).await;
        });

        let bootnode_connector = BootnodeConnector::new(self.bootnodes);

        Ok(BootNode {
            swarm,
            identity,
            peer_manager,
            address_manager,
            kademlia,
            bootnode_connector,
            listen_addrs: self.listen_addrs,
            discovery_tx,
        })
    }
}
