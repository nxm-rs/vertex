//! SwarmNode - the main entry point for Swarm network participation.
//!
//! This module provides [`SwarmNode<N>`] which owns the libp2p swarm and
//! coordinates all network activity. The type parameter `N` determines
//! node capabilities at compile-time via the [`NodeTypes`] trait hierarchy.
//!
//! # Architecture
//!
//! ```text
//! SwarmNode<N: NodeTypes>
//! ├── swarm: Swarm<NodeBehaviour>
//! ├── identity: Arc<SwarmIdentity>
//! ├── peer_manager: Arc<PeerManager>       // Bridge: PeerId ↔ OverlayAddress
//! ├── kademlia: Arc<KademliaTopology>
//! ├── bootnode_connector: BootnodeConnector
//! └── config: NetworkConfig
//!
//! Event loop:
//! ├── Routes TopologyEvent internally (peer activation via PeerManager)
//! └── Routes ClientEvent to ClientService via channel
//! ```
//!
//! # Abstraction Boundary
//!
//! The SwarmNode serves as the bridge between:
//! - **libp2p layer**: Uses PeerId, Multiaddr, ConnectionId
//! - **Swarm layer**: Uses OverlayAddress
//!
//! The `PeerManager` encapsulates the PeerId ↔ OverlayAddress mapping.
//! Kademlia only sees OverlayAddress.
//!
//! # Type Safety
//!
//! The type parameter `N: NodeTypes` provides compile-time guarantees about
//! node capabilities without requiring the underlying libp2p behaviour to
//! be generic. This allows access to associated types like:
//! - `N::Spec` - Network specification
//! - `N::ChunkSet` - Supported chunk types
//! - `N::Topology` - Routing topology
//! - `N::DataAvailability` - Bandwidth accounting
//!
//! # Usage
//!
//! ```ignore
//! use vertex_client_core::{SwarmNode, NetworkConfig};
//! use vertex_swarm_identity::SwarmIdentity;
//!
//! // Create identity
//! let identity = SwarmIdentity::random(spec.clone(), true);
//!
//! // Build node with specific NodeTypes
//! let (node, service, handle) = SwarmNode::<MyNodeTypes>::builder(identity)
//!     .with_config(config)
//!     .build()
//!     .await?;
//!
//! // Run
//! node.run().await?;
//! ```

use std::{sync::Arc, time::Duration};

use eyre::Result;
use futures::StreamExt;
use libp2p::{
    Multiaddr, PeerId, Swarm, SwarmBuilder, identify, noise, swarm::SwarmEvent, tcp, yamux,
};
use nectar_primitives::SwarmAddress;
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};
use vertex_client_kademlia::{KademliaConfig, KademliaTopology};
use vertex_client_peermanager::{
    DiscoveredPeer, DiscoverySender, InternalPeerManager, PeerManager, PeerStore,
    discovery_channel, run_peer_store_consumer,
};
use vertex_net_client::ClientCommand;
use vertex_net_primitives::dns::is_dnsaddr;
use vertex_net_primitives_traits::NodeAddress as NodeAddressTrait;
use vertex_net_topology::{BootnodeConnector, TopologyCommand, TopologyEvent};
use vertex_primitives::OverlayAddress;
use vertex_swarm_api::{Identity, SpawnableTask, SwarmNodeTypes, Topology};
use vertex_tasks::TaskExecutor;

use crate::{
    BootnodeProvider, ClientEvent, ClientHandle, ClientService,
    behaviour::{NodeEvent, SwarmNodeBehaviour},
};

/// A Swarm node generic over the node type hierarchy.
///
/// The type parameter `N` determines capabilities at compile-time:
/// - `N: SwarmNodeTypes` - can retrieve chunks (light client)
/// - `N: SwarmPublisherNodeTypes` - can also publish chunks
/// - `N: SwarmFullNodeTypes` - can also store and sync chunks
pub struct SwarmNode<N: SwarmNodeTypes> {
    /// The libp2p swarm.
    swarm: Swarm<SwarmNodeBehaviour<N>>,

    /// Swarm identity.
    identity: Arc<N::Identity>,

    /// Peer manager for PeerId ↔ OverlayAddress mapping.
    peer_manager: Arc<PeerManager>,

    /// Kademlia topology for peer management (OverlayAddress only).
    kademlia: Arc<KademliaTopology<N::Identity>>,

    /// Bootnode connector.
    bootnode_connector: BootnodeConnector,

    /// Listen addresses for incoming connections.
    listen_addrs: Vec<Multiaddr>,

    /// Channel to send events to the client service.
    client_event_tx: mpsc::UnboundedSender<ClientEvent>,

    /// Channel to receive commands from the client service.
    client_command_rx: mpsc::UnboundedReceiver<ClientCommand>,

    /// Channel to send discovered peers for persistence.
    discovery_tx: DiscoverySender,
}

impl<N: SwarmNodeTypes> SwarmNode<N> {
    /// Create a builder for constructing a SwarmNode.
    pub fn builder(identity: N::Identity) -> SwarmNodeBuilder<N> {
        SwarmNodeBuilder::new(identity)
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

    /// Dial peers from multiaddr strings.
    ///
    /// This is the preferred method for external callers - it keeps libp2p
    /// types internal to the swarm layer.
    ///
    /// Returns the number of successfully initiated dials.
    pub fn dial_addresses(&mut self, addrs: &[String]) -> usize {
        let mut dialed = 0;
        for addr_str in addrs {
            match addr_str.parse::<Multiaddr>() {
                Ok(addr) => {
                    debug!(%addr, "Dialing peer");
                    self.swarm
                        .behaviour_mut()
                        .topology
                        .on_command(TopologyCommand::Dial(addr));
                    dialed += 1;
                }
                Err(e) => {
                    warn!(addr = %addr_str, %e, "Invalid multiaddr, skipping");
                }
            }
        }
        dialed
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
    ///
    /// This dials the configured bootnodes. DNS addresses like `/dnsaddr/mainnet.ethswarm.org`
    /// are resolved automatically by libp2p's DNS transport.
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

    /// Consume self and run as a spawnable future.
    ///
    /// This is the preferred entry point for spawning the node as a background task.
    /// It combines `start_listening()`, `connect_bootnodes()`, and `run()` into a
    /// single owned future suitable for `TaskExecutor::spawn_critical()`.
    pub async fn into_task(mut self) -> Result<()> {
        self.start_listening()?;
        self.connect_bootnodes().await?;
        self.run().await
    }

    /// Run the network event loop.
    ///
    /// This processes swarm events and should be run in a background task.
    /// Prefer [`into_task()`](Self::into_task) which handles setup and takes ownership.
    pub async fn run(mut self) -> Result<()> {
        info!("Starting network event loop");

        loop {
            tokio::select! {
                // Handle swarm events
                event = self.swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                }

                // Handle commands from the client service
                Some(command) = self.client_command_rx.recv() => {
                    self.handle_client_command(command);
                }
            }
        }
    }

    /// Handle a swarm event.
    fn handle_swarm_event(&mut self, event: SwarmEvent<NodeEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(%address, "New listen address");
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
                info!(
                    %peer_id,
                    num_established,
                    cause = ?cause,
                    "Connection closed"
                );
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
                    // Notify peer manager of connection failure
                    // The peer manager handles PeerId -> OverlayAddress mapping internally
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

    /// Handle behaviour-specific events.
    fn handle_behaviour_event(&mut self, event: NodeEvent) {
        match event {
            NodeEvent::Identify(identify::Event::Received { peer_id, info, .. }) => {
                debug!(
                    %peer_id,
                    protocol_version = %info.protocol_version,
                    agent_version = %info.agent_version,
                    "Received identify info"
                );
            }
            NodeEvent::Identify(identify::Event::Sent { peer_id, .. }) => {
                debug!(%peer_id, "Sent identify info");
            }
            NodeEvent::Identify(identify::Event::Pushed { peer_id, .. }) => {
                debug!(%peer_id, "Pushed identify info");
            }
            NodeEvent::Identify(identify::Event::Error { peer_id, error, .. }) => {
                warn!(%peer_id, %error, "Identify error");
            }
            NodeEvent::Topology(event) => {
                self.handle_topology_event(event);
            }
            NodeEvent::Client(event) => {
                self.route_client_event(event);
            }
        }
    }

    /// Handle topology events (bridge layer).
    ///
    /// This is where we translate libp2p events (PeerId) to Swarm events (OverlayAddress)
    /// using the PeerManager bridge.
    fn handle_topology_event(&mut self, event: TopologyEvent) {
        match event {
            TopologyEvent::PeerAuthenticated {
                peer_id,
                connection_id: _,
                info,
            } => {
                // Extract overlay from handshake info
                let overlay = OverlayAddress::new(info.ack.node_address().overlay_address().into());
                let is_full_node = info.ack.full_node();

                debug!(
                    %peer_id,
                    %overlay,
                    %is_full_node,
                    "Peer authenticated after handshake"
                );

                // Bridge: Register PeerId ↔ OverlayAddress mapping
                self.peer_manager
                    .on_peer_ready(peer_id, overlay, is_full_node);

                // Notify Kademlia (OverlayAddress only)
                self.kademlia.connected(overlay);

                // Activate the client handler for this peer
                self.swarm
                    .behaviour_mut()
                    .client
                    .on_command(ClientCommand::ActivatePeer {
                        peer_id,
                        overlay,
                        is_full_node,
                    });
            }
            TopologyEvent::PeerConnectionClosed { peer_id } => {
                // Bridge: PeerId -> OverlayAddress
                if let Some(overlay) = self.peer_manager.on_peer_disconnected(&peer_id) {
                    debug!(%peer_id, %overlay, "Peer disconnected");
                    self.kademlia.disconnected(&overlay);
                } else {
                    debug!(%peer_id, "Peer disconnected (overlay unknown)");
                }
            }
            TopologyEvent::HivePeersReceived { from, peers } => {
                debug!(%from, count = peers.len(), "Received peers via hive");

                // Extract overlays and cache underlays for dialing
                let mut overlays = Vec::with_capacity(peers.len());
                let mut underlay_entries = Vec::with_capacity(peers.len());

                for bzz in &peers {
                    let overlay = OverlayAddress::from(bzz.overlay);
                    overlays.push(overlay);
                    underlay_entries.push((overlay, bzz.underlays.clone()));
                }

                // Cache underlays synchronously (needed for dialing)
                self.peer_manager.cache_underlays_batch(underlay_entries);

                // Send to discovery channel for persistence (async)
                for bzz in peers {
                    let peer =
                        DiscoveredPeer::new(bzz.overlay, bzz.underlays, bzz.signature, bzz.nonce);
                    if let Err(e) = self.discovery_tx.send(peer) {
                        trace!(error = %e, "discovery channel full or closed");
                    }
                }

                // Add to kademlia (OverlayAddress only)
                self.kademlia.add_peers(&overlays);

                // Evaluate candidates immediately and dial them
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

    /// Dial peers that the Kademlia topology suggests we should connect to.
    fn dial_connection_candidates(&mut self) {
        let candidates = self.kademlia.peers_to_connect();

        // Batch filter: get dialable candidates with cached underlays (single lock acquisition)
        let dialable = self.peer_manager.filter_dialable_candidates(&candidates);

        for (overlay, underlays) in dialable {
            // Find an address with peer_id
            let Some((addr, peer_id)) = underlays.iter().find_map(|addr| {
                addr.iter().find_map(|p| {
                    if let libp2p::multiaddr::Protocol::P2p(id) = p {
                        Some((addr.clone(), id))
                    } else {
                        None
                    }
                })
            }) else {
                debug!(%overlay, "No underlay with peer_id found");
                continue;
            };

            // Skip if already connected at libp2p level
            if self.swarm.is_connected(&peer_id) {
                continue;
            }

            debug!(%overlay, %addr, %peer_id, "Dialing discovered peer");

            // Mark as connecting in peer manager
            if !self.peer_manager.start_connecting(overlay) {
                continue;
            }

            // Mark as connecting in kademlia
            self.kademlia.start_connecting(overlay);

            // Dial
            if let Err(e) = self.swarm.dial(addr.clone()) {
                debug!(%overlay, %addr, %e, "Failed to dial discovered peer");
                self.peer_manager.connection_failed(&overlay);
            }
        }
    }

    /// Route a client event to the client service.
    fn route_client_event(&self, event: ClientEvent) {
        if let Err(e) = self.client_event_tx.send(event) {
            warn!(%e, "Failed to send client event to service");
        }
    }

    /// Handle a command from the client service.
    fn handle_client_command(&mut self, command: ClientCommand) {
        self.swarm.behaviour_mut().client.on_command(command);
    }

    /// Get the number of connected peers.
    pub fn connected_peers(&self) -> usize {
        self.swarm.connected_peers().count()
    }

    /// Check if we're connected to any peers.
    pub fn is_connected(&self) -> bool {
        self.connected_peers() > 0
    }
}

impl<N: SwarmNodeTypes> SpawnableTask for SwarmNode<N> {
    fn spawn_task(self) -> impl std::future::Future<Output = ()> + Send {
        async move {
            if let Err(e) = self.into_task().await {
                tracing::error!(error = %e, "SwarmNode error");
            }
        }
    }
}

/// Builder for SwarmNode with fluent configuration.
pub struct SwarmNodeBuilder<N: SwarmNodeTypes> {
    identity: N::Identity,
    listen_addrs: Vec<Multiaddr>,
    bootnodes: Vec<Multiaddr>,
    idle_timeout: Duration,
    kademlia_config: KademliaConfig,
    peer_store: Option<Arc<dyn PeerStore>>,
}

impl<N: SwarmNodeTypes> SwarmNodeBuilder<N> {
    /// Create a new builder with the given identity.
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
        }
    }

    /// Set network configuration from a NetworkConfig implementation.
    ///
    /// If no bootnodes are provided in the config, falls back to spec bootnodes.
    pub fn with_network_config(mut self, config: &impl vertex_swarm_api::NetworkConfig) -> Self {
        use vertex_swarm_api::Identity;

        self.listen_addrs = config
            .listen_addrs()
            .into_iter()
            .filter_map(|s| s.parse().ok())
            .collect();

        // Use config bootnodes, or fall back to spec bootnodes
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
        self
    }

    /// Set the bootnodes.
    pub fn with_bootnodes(mut self, bootnodes: Vec<Multiaddr>) -> Self {
        self.bootnodes = bootnodes;
        self
    }

    /// Set the listen addresses.
    pub fn with_listen_addrs(mut self, addrs: Vec<Multiaddr>) -> Self {
        self.listen_addrs = addrs;
        self
    }

    /// Set the Kademlia configuration.
    pub fn with_kademlia_config(mut self, config: KademliaConfig) -> Self {
        self.kademlia_config = config;
        self
    }

    /// Set the peer store for persistence.
    ///
    /// When set, peers will be loaded from and persisted to the store.
    pub fn with_peer_store(mut self, store: Arc<dyn PeerStore>) -> Self {
        self.peer_store = Some(store);
        self
    }

    /// Build the SwarmNode and ClientService.
    ///
    /// Returns the node and a client service that should be spawned as a background task.
    pub async fn build(self) -> Result<(SwarmNode<N>, ClientService, ClientHandle)> {
        info!("Initializing P2P network...");

        let identity = Arc::new(self.identity);
        let identity_for_behaviour = identity.clone();
        let identity_for_kademlia = N::Identity::clone(&identity);

        // Build the swarm with DNS-enabled transport
        let swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_dns()?
            .with_behaviour(|keypair| {
                Ok(SwarmNodeBehaviour::new(
                    keypair.public().clone(),
                    identity_for_behaviour.clone(),
                ))
            })?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(self.idle_timeout))
            .build();

        let local_peer_id = *swarm.local_peer_id();
        info!(%local_peer_id, "Local peer ID");
        info!(overlay = %identity.overlay_address(), "Overlay address");

        // Create the peer manager (bridge layer)
        let peer_manager = match self.peer_store {
            Some(store) => {
                let pm = PeerManager::with_store(store)
                    .map_err(|e| eyre::eyre!("failed to initialize peer manager: {}", e))?;
                info!(count = pm.stats().stored_peers, "loaded peers from store");
                Arc::new(pm)
            }
            None => Arc::new(PeerManager::new()),
        };

        // Create the Kademlia topology and spawn its manage loop
        let kademlia = KademliaTopology::new(identity_for_kademlia, self.kademlia_config);

        // Seed Kademlia with known dialable peers from the store
        let known_peers = peer_manager.known_dialable_peers();
        if !known_peers.is_empty() {
            info!(
                count = known_peers.len(),
                "seeding kademlia with stored peers"
            );
            kademlia.add_peers(&known_peers);
        }

        // Use global task executor for manage loop (set by TaskManager at node startup)
        let executor = TaskExecutor::current();
        let _manage_handle = kademlia.clone().spawn_manage_loop(&executor);

        // Spawn stats reporting task
        let _stats_handle = crate::stats::spawn_stats_task(
            kademlia.clone(),
            crate::stats::StatsConfig::default(),
            &executor,
        );

        // Create discovery channel for peer persistence
        let (discovery_tx, discovery_rx) = discovery_channel();

        // Spawn peer store consumer task (persists discovered peers)
        let pm_for_consumer = peer_manager.clone();
        executor.spawn(async move {
            run_peer_store_consumer(pm_for_consumer, discovery_rx).await;
        });

        let bootnode_connector = BootnodeConnector::new(self.bootnodes);

        // Create channels for client communication
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Create the client service with channels
        let (client_service, client_handle) = ClientService::with_channels(command_tx, event_rx);

        let node = SwarmNode {
            swarm,
            identity,
            peer_manager,
            kademlia,
            bootnode_connector,
            listen_addrs: self.listen_addrs,
            client_event_tx: event_tx,
            client_command_rx: command_rx,
            discovery_tx,
        };

        Ok((node, client_service, client_handle))
    }
}

/// Swarm node type determines what capabilities and protocols the node runs.
///
/// Each type builds on the capabilities of the previous:
/// - Bootnode: Only topology (Hive/Kademlia)
/// - Light: + Bandwidth accounting + Retrieval
/// - Publisher: + Upload/Postage
/// - Full: + Pullsync + Local storage
/// - Staker: + Redistribution game
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, strum::Display)]
#[cfg_attr(
    feature = "cli",
    derive(clap::ValueEnum, serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(feature = "cli", serde(rename_all = "lowercase"))]
pub enum SwarmNodeType {
    /// Bootnode - only participates in topology (Kademlia/Hive).
    Bootnode,

    /// Light node - can retrieve chunks from the network.
    #[default]
    Light,

    /// Publisher node - can retrieve + upload chunks.
    Publisher,

    /// Full node - stores chunks for the network.
    Full,

    /// Staker node - full storage with redistribution rewards.
    Staker,
}

impl SwarmNodeType {
    /// Check if this node type requires availability accounting.
    pub fn requires_availability(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    /// Check if this node type requires retrieval protocol.
    pub fn requires_retrieval(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    /// Check if this node type requires upload/postage.
    pub fn requires_upload(&self) -> bool {
        matches!(
            self,
            SwarmNodeType::Publisher | SwarmNodeType::Full | SwarmNodeType::Staker
        )
    }

    /// Check if this node type requires pullsync.
    pub fn requires_pullsync(&self) -> bool {
        matches!(self, SwarmNodeType::Full | SwarmNodeType::Staker)
    }

    /// Check if this node type requires local storage.
    pub fn requires_storage(&self) -> bool {
        matches!(self, SwarmNodeType::Full | SwarmNodeType::Staker)
    }

    /// Check if this node type requires redistribution.
    pub fn requires_redistribution(&self) -> bool {
        matches!(self, SwarmNodeType::Staker)
    }

    /// Check if this node type requires persistent identity.
    pub fn requires_persistent_identity(&self) -> bool {
        matches!(self, SwarmNodeType::Full | SwarmNodeType::Staker)
    }

    /// Check if this node type requires persistent nonce (stable overlay).
    pub fn requires_persistent_nonce(&self) -> bool {
        matches!(self, SwarmNodeType::Full | SwarmNodeType::Staker)
    }
}
