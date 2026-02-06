//! Unified topology service owning all peer-related state.
//!
//! # Ownership Model
//!
//! ```text
//! TopologyService ─┬─ owns Arc<KademliaRouting>
//!                  ├─ owns Arc<PeerManager>
//!                  ├─ owns Arc<DialTracker>
//!                  ├─ owns Arc<NatDiscovery>
//!                  └─ owns event_tx (broadcast::Sender)
//!                         │
//!    ┌────────────────────┴────────────────────┐
//!    ↓                                         ↓
//! TopologyHandle                        TopologyBehaviour
//! (query facade)                        (libp2p behaviour)
//! ```
//!
//! All internal components are created and owned by `TopologyService`. External
//! code interacts only through `TopologyHandle` for queries and commands.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use libp2p::Multiaddr;
use tokio::sync::{broadcast, mpsc};
use tracing::info;
use vertex_swarm_api::{PeerConfigValues, SwarmBootnodeConfig, SwarmIdentity, SwarmTopology};

#[cfg(test)]
use vertex_swarm_api::{DefaultPeerConfig, SwarmNetworkConfig, SwarmPeerConfig};
use vertex_net_local::LocalCapabilities;
use vertex_swarm_peermanager::PeerManager;
use vertex_swarm_primitives::OverlayAddress;

use crate::nat_discovery::{NatDiscovery, NatDiscoveryConfig};

use crate::gossip_coordinator::DepthProvider;
use crate::routing::PeerFailureProvider;
use crate::bootnode::BootnodeConnector;
use crate::dial_tracker::DialTracker;
use crate::error::TopologyError;
use crate::events::TopologyServiceEvent;
use crate::handle::TopologyHandle;
use crate::routing::{KademliaConfig, KademliaRouting};

/// Event channel capacity (bounded to prevent memory growth under slow consumers).
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Command channel capacity.
const COMMAND_CHANNEL_CAPACITY: usize = 64;

/// Topology-specific configuration (Kademlia routing, gossip, dial interval).
///
/// Network addresses (bootnodes, listen addrs, NAT) come from `SwarmNetworkConfig`.
#[derive(Debug, Clone, Default)]
pub struct TopologyServiceConfig {
    /// Kademlia routing configuration.
    pub kademlia: KademliaConfig,
    /// Dial candidate check interval (None for default 5s, Some(ZERO) disables).
    pub dial_interval: Option<std::time::Duration>,
    /// Gossip configuration (None disables gossip, Some enables with config).
    pub gossip: Option<crate::gossip::HiveGossipConfig>,
}

impl TopologyServiceConfig {
    /// Create with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set Kademlia routing configuration.
    pub fn with_kademlia(mut self, config: KademliaConfig) -> Self {
        self.kademlia = config;
        self
    }

    /// Set dial candidate check interval. Pass `Duration::ZERO` to disable dial polling.
    pub fn with_dial_interval(mut self, interval: std::time::Duration) -> Self {
        self.dial_interval = Some(interval);
        self
    }

    /// Enable gossip with the given configuration.
    pub fn with_gossip(mut self, config: crate::gossip::HiveGossipConfig) -> Self {
        self.gossip = Some(config);
        self
    }

    /// Enable gossip with default configuration.
    pub fn with_gossip_enabled(mut self) -> Self {
        self.gossip = Some(crate::gossip::HiveGossipConfig::default());
        self
    }
}

/// Implement PeerFailureProvider for PeerManager to delegate failure tracking.
impl PeerFailureProvider for PeerManager {
    fn failure_score(&self, peer: &OverlayAddress) -> f64 {
        self.peer_score(peer)
    }

    fn record_failure(&self, peer: &OverlayAddress) {
        // Adjust score for failure (negative delta)
        self.adjust_score(peer, -1.0);
    }
}

/// Unified topology service owning all peer-related state.
pub struct TopologyService<I: SwarmIdentity> {
    routing: Arc<KademliaRouting<I>>,
    peer_manager: Arc<PeerManager>,
    /// Kept for ownership - shared via TopologyBehaviourComponents.
    #[allow(dead_code)]
    dial_tracker: Arc<DialTracker>,
    /// Kept for ownership - shared via TopologyBehaviourComponents.
    #[allow(dead_code)]
    local_capabilities: Arc<LocalCapabilities>,
    /// Kept for ownership - shared via TopologyBehaviourComponents.
    #[allow(dead_code)]
    nat_discovery: Arc<NatDiscovery>,
    bootnode_connector: BootnodeConnector,
    trusted_peers: Vec<Multiaddr>,
    event_tx: broadcast::Sender<TopologyServiceEvent>,
    shutdown: AtomicBool,
    listen_addrs: Vec<Multiaddr>,
}

use crate::TopologyCommand;

/// Command receiver for TopologyBehaviour to poll.
pub type CommandReceiver = mpsc::Receiver<TopologyCommand>;

/// Components for constructing TopologyBehaviour. Consumed via `into_behaviour()`.
pub struct TopologyBehaviourComponents<I: SwarmIdentity> {
    pub(crate) identity: I,
    pub(crate) peer_manager: Arc<PeerManager>,
    pub(crate) routing: Arc<KademliaRouting<I>>,
    pub(crate) event_tx: broadcast::Sender<TopologyServiceEvent>,
    pub(crate) dial_tracker: Arc<DialTracker>,
    pub(crate) command_rx: CommandReceiver,
    pub(crate) nat_discovery: Arc<NatDiscovery>,
    pub(crate) dial_interval: Option<std::time::Duration>,
    pub(crate) gossip: Option<crate::gossip::HiveGossipConfig>,
}

impl<I: SwarmIdentity> TopologyBehaviourComponents<I> {
    /// Consume components to create TopologyBehaviour, auto-enabling gossip if configured.
    pub fn into_behaviour(
        self,
        config: crate::handler::TopologyConfig,
    ) -> (crate::behaviour::TopologyBehaviour<I>, DepthProvider) {
        let depth_provider: DepthProvider = {
            let routing = self.routing.clone();
            Arc::new(move || routing.depth())
        };

        let mut behaviour = crate::behaviour::TopologyBehaviour::new(
            self.identity,
            config,
            self.peer_manager,
            self.routing,
            self.event_tx,
            self.dial_tracker,
            self.command_rx,
            self.nat_discovery,
            self.dial_interval,
        );

        // Auto-enable gossip if config was provided
        if let Some(gossip_config) = self.gossip {
            behaviour.enable_gossip(gossip_config, depth_provider.clone());
        }

        (behaviour, depth_provider)
    }
}

impl<I: SwarmIdentity> std::fmt::Debug for TopologyBehaviourComponents<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopologyBehaviourComponents").finish_non_exhaustive()
    }
}

impl<I: SwarmIdentity> TopologyService<I> {
    /// Create a new topology service, handle, and behaviour components.
    ///
    /// Network addresses (bootnodes, listen, NAT) come from `config`.
    /// Topology-specific settings (Kademlia, gossip) come from `topology_config`.
    ///
    /// Identity must be `Clone` (typically `Arc<Identity>`) for sharing across components.
    pub fn new(
        identity: I,
        config: &impl SwarmBootnodeConfig,
        topology_config: TopologyServiceConfig,
    ) -> Result<(Self, TopologyHandle<I>, TopologyBehaviourComponents<I>), TopologyError>
    where
        I: Clone,
    {
        // Get pre-parsed multiaddrs from network config
        let bootnodes = config.bootnodes().to_vec();
        let trusted_peers = config.trusted_peers().to_vec();
        let listen_addrs = config.listen_addrs().to_vec();
        let nat_addrs = config.nat_addrs().to_vec();
        let nat_auto = config.nat_auto_enabled();
        // Peer config values
        let peer_store_path = config.peers().store_path();
        let peer_ban_threshold = config.peers().ban_threshold();
        let peer_store_limit = config.peers().store_limit();

        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);

        // Create peer manager (from path if provided)
        let peer_manager =
            Self::create_peer_manager(peer_store_path.as_ref(), peer_ban_threshold, peer_store_limit)?;

        // Create Kademlia routing with failure provider (unified failure tracking)
        let routing = KademliaRouting::with_failure_provider(
            identity.clone(),
            topology_config.kademlia,
            peer_manager.clone(),
        );

        // Seed kademlia with stored peers
        let known_peers = peer_manager.disconnected_peers();
        if !known_peers.is_empty() {
            info!(
                count = known_peers.len(),
                "seeding kademlia with stored peers"
            );
            routing.add_peers(&known_peers);
        }

        // Create bootnode connector
        let bootnode_connector = BootnodeConnector::new(bootnodes);

        // Create dial tracker
        let dial_tracker = Arc::new(DialTracker::new());

        // Create local capabilities (protocol-agnostic address tracking)
        let local_capabilities = Arc::new(LocalCapabilities::new());

        // Log NAT addresses
        if !nat_addrs.is_empty() {
            info!(count = nat_addrs.len(), "NAT addresses configured");
            for addr in &nat_addrs {
                info!(%addr, "NAT address");
            }
        }

        // Create NAT discovery (Swarm-specific observed address confirmation)
        let nat_discovery = Arc::new(if nat_auto {
            info!("Auto NAT discovery enabled");
            NatDiscovery::new(
                local_capabilities.clone(),
                nat_addrs,
                NatDiscoveryConfig::default(),
                true,
            )
        } else {
            NatDiscovery::new(
                local_capabilities.clone(),
                nat_addrs,
                NatDiscoveryConfig::default(),
                false,
            )
        });

        let shutdown = AtomicBool::new(false);

        let handle = TopologyHandle::new(
            routing.clone(),
            peer_manager.clone(),
            dial_tracker.clone(),
            command_tx,
            event_tx.clone(),
        );

        let service = Self {
            routing: routing.clone(),
            peer_manager: peer_manager.clone(),
            dial_tracker: dial_tracker.clone(),
            local_capabilities: local_capabilities.clone(),
            nat_discovery: nat_discovery.clone(),
            bootnode_connector,
            trusted_peers,
            event_tx: event_tx.clone(),
            shutdown,
            listen_addrs,
        };

        // Create behaviour components (consumed during behaviour construction)
        let components = TopologyBehaviourComponents {
            identity,
            peer_manager,
            routing,
            event_tx,
            dial_tracker,
            command_rx,
            nat_discovery,
            dial_interval: topology_config.dial_interval,
            gossip: topology_config.gossip,
        };

        Ok((service, handle, components))
    }

    /// Create peer manager, optionally with file-based persistence.
    fn create_peer_manager(
        store_path: Option<&PathBuf>,
        ban_threshold: f64,
        max_peers: Option<usize>,
    ) -> Result<Arc<PeerManager>, TopologyError> {
        use vertex_swarm_peermanager::FilePeerStore;

        match store_path {
            Some(path) => {
                let store = FilePeerStore::new_with_create_dir(path).map_err(|e| {
                    TopologyError::PeerStoreCreation {
                        path: path.clone(),
                        reason: e.to_string(),
                    }
                })?;

                match PeerManager::with_store_and_limits(Arc::new(store), ban_threshold, max_peers) {
                    Ok(pm) => {
                        info!(count = pm.stats().total_peers, path = %path.display(), "loaded peers from store");
                        Ok(Arc::new(pm))
                    }
                    Err(e) => Err(TopologyError::PeerStoreLoad {
                        reason: e.to_string(),
                    }),
                }
            }
            None => Ok(Arc::new(PeerManager::with_limits(ban_threshold, max_peers))),
        }
    }

    /// Connect to configured bootnodes. Returns the number of dials initiated.
    pub fn connect_bootnodes<F, E>(&self, mut dial_fn: F) -> usize
    where
        F: FnMut(Multiaddr) -> Result<(), E>,
        E: std::fmt::Display,
    {
        let bootnodes = self.bootnode_connector.shuffled_bootnodes();

        if bootnodes.is_empty() {
            tracing::warn!("No bootnodes configured");
            return 0;
        }

        info!(count = bootnodes.len(), "Connecting to bootnodes...");

        let mut connected = 0;
        let min_connections = self.bootnode_connector.min_connections();

        for bootnode in bootnodes {
            if connected >= min_connections {
                info!(connected, "Reached minimum bootnode connections");
                break;
            }

            let is_dns = crate::dns::is_dnsaddr(&bootnode);
            info!(
                %bootnode,
                is_dnsaddr = is_dns,
                "Dialing bootnode{}",
                if is_dns { " (dnsaddr will be resolved)" } else { "" }
            );

            match dial_fn(bootnode.clone()) {
                Ok(_) => {
                    tracing::debug!(%bootnode, "Dial initiated");
                    connected += 1;
                }
                Err(e) => {
                    tracing::warn!(%bootnode, error = %e, "Failed to dial bootnode");
                }
            }
        }

        connected
    }

    /// Check if any bootnodes are configured.
    pub fn has_bootnodes(&self) -> bool {
        self.bootnode_connector.has_bootnodes()
    }

    /// Get the trusted peers addresses.
    pub fn trusted_peers(&self) -> &[Multiaddr] {
        &self.trusted_peers
    }

    /// Check if any trusted peers are configured.
    pub fn has_trusted_peers(&self) -> bool {
        !self.trusted_peers.is_empty()
    }

    /// Connect to configured trusted peers. Returns the number of dials initiated.
    pub fn connect_trusted_peers<F, E>(&self, mut dial_fn: F) -> usize
    where
        F: FnMut(Multiaddr) -> Result<(), E>,
        E: std::fmt::Display,
    {
        if self.trusted_peers.is_empty() {
            return 0;
        }

        info!(count = self.trusted_peers.len(), "Connecting to trusted peers...");

        let mut connected = 0;
        for peer in &self.trusted_peers {
            match dial_fn(peer.clone()) {
                Ok(_) => {
                    tracing::debug!(%peer, "Trusted peer dial initiated");
                    connected += 1;
                }
                Err(e) => {
                    tracing::warn!(%peer, error = %e, "Failed to dial trusted peer");
                }
            }
        }
        connected
    }

    /// Get the listen addresses.
    pub fn listen_addrs(&self) -> &[Multiaddr] {
        &self.listen_addrs
    }

    /// Emit an event to all subscribers.
    pub fn emit_event(&self, event: TopologyServiceEvent) {
        // Broadcast returns error if no receivers, which is fine
        let _ = self.event_tx.send(event);
    }

    /// Check if the service is shut down.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Signal shutdown.
    pub fn shutdown(&self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Save all peers to the peer store.
    ///
    /// Call this on shutdown to persist peer state.
    pub fn save_peers(&self) -> Result<usize, String> {
        self.peer_manager
            .save_all_to_store()
            .map_err(|e| e.to_string())
    }
}

impl<I: SwarmIdentity> std::fmt::Debug for TopologyService<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopologyService")
            .field("depth", &self.routing.depth())
            .field("connected_peers", &self.routing.connected_peers().len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use alloy_signer_local::LocalSigner;
    use nectar_primitives::SwarmAddress;
    use vertex_swarm_api::SwarmTopologyProvider;
    use vertex_swarm_primitives::OverlayAddress;
    use vertex_swarm_spec::Spec;
    use vertex_tasks::{TaskExecutor, TaskManager};

    #[derive(Clone)]
    struct MockIdentity {
        overlay: SwarmAddress,
        signer: Arc<LocalSigner<alloy_signer::k256::ecdsa::SigningKey>>,
        spec: Arc<Spec>,
    }

    impl std::fmt::Debug for MockIdentity {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MockIdentity")
                .field("overlay", &self.overlay)
                .finish_non_exhaustive()
        }
    }

    impl MockIdentity {
        fn with_overlay(overlay: OverlayAddress) -> Self {
            let signer = LocalSigner::random();
            Self {
                overlay,
                signer: Arc::new(signer),
                spec: vertex_swarm_spec::init_testnet(),
            }
        }
    }

    impl SwarmIdentity for MockIdentity {
        type Spec = Spec;
        type Signer = LocalSigner<alloy_signer::k256::ecdsa::SigningKey>;

        fn spec(&self) -> &Self::Spec {
            &self.spec
        }

        fn nonce(&self) -> B256 {
            B256::ZERO
        }

        fn signer(&self) -> Arc<Self::Signer> {
            self.signer.clone()
        }

        fn node_type(&self) -> vertex_swarm_api::SwarmNodeType {
            vertex_swarm_api::SwarmNodeType::Storer
        }

        fn overlay_address(&self) -> SwarmAddress {
            self.overlay
        }
    }

    fn addr_from_byte(b: u8) -> OverlayAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = b;
        OverlayAddress::from(bytes)
    }

    /// Create a test executor using a fresh runtime.
    fn test_executor() -> (TaskExecutor, TaskManager) {
        let manager = TaskManager::current();
        let executor = manager.executor();
        (executor, manager)
    }

    /// Test network config implementing SwarmNetworkConfig for tests.
    #[derive(Debug, Clone, Default)]
    struct TestNetworkConfig {
        listen_addrs: Vec<Multiaddr>,
        bootnodes: Vec<Multiaddr>,
    }

    impl SwarmNetworkConfig for TestNetworkConfig {
        fn listen_addrs(&self) -> &[Multiaddr] {
            &self.listen_addrs
        }
        fn bootnodes(&self) -> &[Multiaddr] {
            &self.bootnodes
        }
        fn discovery_enabled(&self) -> bool {
            true
        }
        fn max_peers(&self) -> usize {
            50
        }
        fn idle_timeout(&self) -> std::time::Duration {
            std::time::Duration::from_secs(30)
        }
    }

    impl SwarmPeerConfig for TestNetworkConfig {
        type Peers = DefaultPeerConfig;

        fn peers(&self) -> &Self::Peers {
            // Use static default since tests don't need custom peer config
            static DEFAULT: DefaultPeerConfig = DefaultPeerConfig {
                ban_threshold: -100.0,
                store_limit: Some(10_000),
                store_path: None,
            };
            &DEFAULT
        }
    }

    #[tokio::test]
    async fn test_topology_service_creation() {
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (service, handle, _cmd_rx) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        assert_eq!(handle.identity().overlay_address(), base);
        assert_eq!(handle.depth(), 0);
        assert!(handle.routing.connected_peers().is_empty());
        assert!(!service.is_shutdown());
    }

    #[tokio::test]
    async fn test_topology_handle_clone() {
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (_service, handle, _cmd_rx) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");
        let handle2 = handle.clone();

        assert_eq!(
            handle.identity().overlay_address(),
            handle2.identity().overlay_address()
        );
    }

    #[tokio::test]
    async fn test_topology_handle_peer_queries() {
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (service, handle, _cmd_rx) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        // Add and connect some peers via the routing
        let peer1 = addr_from_byte(0x80);
        let peer2 = addr_from_byte(0x40);

        service.routing.add_peers(&[peer1, peer2]);
        assert_eq!(handle.routing.known_peers().len(), 2);

        service.routing.connected(peer1);
        assert_eq!(handle.routing.connected_peers().len(), 1);
        assert!(handle.routing.connected_peers().contains(&peer1));
    }

    #[tokio::test]
    async fn test_topology_service_event_subscription() {
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (service, handle, _cmd_rx) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        let mut receiver = handle.subscribe();

        // Emit an event
        service.emit_event(TopologyServiceEvent::DepthChanged {
            old_depth: 0,
            new_depth: 5,
        });

        // Should receive the event
        let event = receiver.try_recv().expect("should receive event");
        match event {
            TopologyServiceEvent::DepthChanged {
                old_depth,
                new_depth,
            } => {
                assert_eq!(old_depth, 0);
                assert_eq!(new_depth, 5);
            }
            _ => panic!("unexpected event type"),
        }
    }

    #[tokio::test]
    async fn test_topology_service_shutdown() {
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (service, _handle, _cmd_rx) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        assert!(!service.is_shutdown());
        service.shutdown();
        assert!(service.is_shutdown());
    }

    #[test]
    fn test_topology_service_config() {
        let config = TopologyServiceConfig::new()
            .with_kademlia(KademliaConfig::default().with_low_watermark(3))
            .with_gossip_enabled();

        assert!(config.gossip.is_some());
    }

    #[tokio::test]
    async fn test_handle_encapsulation_no_arc_leaks() {
        // Verifies TopologyHandle only exposes query methods, not raw Arcs.
        // The handle should provide data via methods, never Arc<T> references.
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (_service, handle, _components) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        // Verify all query methods return owned data, not Arc references.
        // These calls should compile and return copies/clones, not Arc<T>.
        let _connected: Vec<OverlayAddress> = handle.routing.connected_peers();
        let _known: Vec<OverlayAddress> = handle.routing.known_peers();
        let _neighbors: Vec<OverlayAddress> = handle.routing.neighbors(0);
        let _depth: u8 = handle.depth();
        let _bin_sizes: Vec<(usize, usize)> = handle.routing.bin_sizes();
        // pending_connections_count accessible via SwarmTopologyProvider trait
        let _pending: usize = SwarmTopologyProvider::pending_connections_count(&handle);

        // Verify identity is returned by reference to the handle's owned clone
        let _identity_ref = handle.identity();
        assert_eq!(_identity_ref.overlay_address(), base);
    }

    #[tokio::test]
    async fn test_handle_clone_shares_state() {
        // Verifies cloned handles see the same underlying state.
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (service, handle1, _components) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        let handle2 = handle1.clone();

        // Both handles should see same initial state
        assert_eq!(handle1.depth(), handle2.depth());
        assert_eq!(
            handle1.routing.connected_peers().len(),
            handle2.routing.connected_peers().len()
        );

        // Add a peer via the service's routing
        let peer = addr_from_byte(0x80);
        service.routing.add_peers(&[peer]);
        service.routing.connected(peer);

        // Both handles should now see the connected peer
        assert_eq!(handle1.routing.connected_peers().len(), 1);
        assert_eq!(handle2.routing.connected_peers().len(), 1);
        assert!(handle1.routing.connected_peers().contains(&peer));
        assert!(handle2.routing.connected_peers().contains(&peer));
    }

    #[tokio::test]
    async fn test_config_flows_to_components() {
        // Verifies configuration flows correctly from SwarmNetworkConfig to internal components.
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let network_config = TestNetworkConfig {
            listen_addrs: vec!["/ip4/0.0.0.0/tcp/1634".parse().unwrap()],
            bootnodes: vec!["/ip4/1.2.3.4/tcp/1634".parse().unwrap()],
        };
        let topology_config =
            TopologyServiceConfig::new().with_kademlia(KademliaConfig::default().with_low_watermark(3));

        let (service, _handle, _components) =
            TopologyService::new(identity, &network_config, topology_config)
                .expect("topology service creation should succeed");

        // Verify bootnodes were stored
        assert!(service.has_bootnodes());

        // Verify listen addrs were stored
        assert_eq!(service.listen_addrs().len(), 1);
    }

    #[tokio::test]
    async fn test_behaviour_components_consumed_once() {
        // Verifies TopologyBehaviourComponents is designed to be consumed once
        // via into_behaviour(), not reused.
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (_service, _handle, components) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        // Components should be consumable (this is a compile-time guarantee via ownership)
        // We verify Debug impl works (doesn't expose internals)
        let debug_str = format!("{:?}", components);
        assert!(debug_str.contains("TopologyBehaviourComponents"));
        // Debug should use finish_non_exhaustive() to hide internals
        assert!(!debug_str.contains("Arc"));
    }

    #[tokio::test]
    async fn test_event_broadcast_multiple_subscribers() {
        // Verifies events are properly broadcast to multiple subscribers.
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (service, handle, _components) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        // Create multiple subscribers
        let mut rx1 = handle.subscribe();
        let mut rx2 = handle.subscribe();

        // Emit an event
        service.emit_event(TopologyServiceEvent::DepthChanged {
            old_depth: 0,
            new_depth: 1,
        });

        // Both subscribers should receive the event
        let event1 = rx1.try_recv().expect("rx1 should receive event");
        let event2 = rx2.try_recv().expect("rx2 should receive event");

        match (event1, event2) {
            (
                TopologyServiceEvent::DepthChanged { new_depth: d1, .. },
                TopologyServiceEvent::DepthChanged { new_depth: d2, .. },
            ) => {
                assert_eq!(d1, 1);
                assert_eq!(d2, 1);
            }
            _ => panic!("unexpected event types"),
        }
    }

    #[tokio::test]
    async fn test_event_broadcast_no_panic_without_subscribers() {
        // Verifies emitting events when no subscribers exist doesn't panic.
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (service, _handle, _components) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        // Emit events without any subscribers - should not panic
        service.emit_event(TopologyServiceEvent::DepthChanged {
            old_depth: 0,
            new_depth: 1,
        });
        service.emit_event(TopologyServiceEvent::PeerDisconnected {
            overlay: addr_from_byte(0x80),
        });
    }

    #[tokio::test]
    async fn test_peer_manager_accessible() {
        use alloy_primitives::{Address, Signature, U256};
        use vertex_swarm_peer::SwarmPeer;

        // Verifies peer manager state is accessible through handle's public field.
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (_service, handle, _components) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        // Create a SwarmPeer to store
        let overlay_byte = 0x80u8;
        let overlay = addr_from_byte(overlay_byte);
        let swarm_peer = SwarmPeer::from_validated(
            vec![format!("/ip4/127.0.0.{}/tcp/1634", overlay_byte).parse().unwrap()],
            Signature::new(U256::ZERO, U256::ZERO, false),
            alloy_primitives::B256::from_slice(overlay.as_slice()),
            alloy_primitives::B256::ZERO,
            Address::ZERO,
        );

        // Store the peer first (peers must exist before operations)
        handle.peer_manager.store_discovered_peer(swarm_peer);

        // Initially peer should have default score and not be banned
        let score = handle.peer_manager.peer_score(&overlay);
        assert_eq!(score, 0.0); // Default score for new peer

        assert!(!handle.peer_manager.is_banned(&overlay));

        // Ban peer through handle (this tests the coordinated method works)
        handle.ban_peer(&overlay, Some("test ban".to_string()));
        assert!(handle.peer_manager.is_banned(&overlay));
    }

    #[tokio::test]
    async fn test_pending_connections_via_trait() {
        // Verifies dial stats are accessible via SwarmTopologyProvider trait.
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (_service, handle, _components) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        // Query via trait method (dial_tracker is private implementation detail)
        assert_eq!(SwarmTopologyProvider::pending_connections_count(&handle), 0);
    }

    #[tokio::test]
    async fn test_routing_state_through_handle() {
        // Verifies all routing state is accessible through handle's public routing field.
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (service, handle, _components) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        // Add peers at different proximity orders
        let peer_po0 = addr_from_byte(0x80); // PO 0 (first bit differs)
        let peer_po1 = addr_from_byte(0x40); // PO 1
        let peer_po2 = addr_from_byte(0x20); // PO 2

        service.routing.add_peers(&[peer_po0, peer_po1, peer_po2]);

        // Verify known_peers through handle's routing field
        assert_eq!(handle.routing.known_peers().len(), 3);

        // Connect peers
        service.routing.connected(peer_po0);
        service.routing.connected(peer_po1);

        // Verify connected_peers through handle's routing field
        assert_eq!(handle.routing.connected_peers().len(), 2);
        assert_eq!(handle.routing.known_peers().len(), 1); // peer_po2 still known but not connected

        // Verify bin_sizes through handle's routing field
        let bins = handle.routing.bin_sizes();
        assert_eq!(bins[0].0, 1); // PO 0: 1 connected
        assert_eq!(bins[1].0, 1); // PO 1: 1 connected
        assert_eq!(bins[2].1, 1); // PO 2: 1 known (not connected)

        // Verify closest_to through handle (kept for convenience)
        let mut target_bytes = [0x00u8; 32];
        target_bytes[0] = 0x81; // Close to peer_po0
        let target = nectar_primitives::ChunkAddress::from(target_bytes);
        let closest = handle.closest_to(&target, 1);
        assert_eq!(closest.len(), 1);
        assert_eq!(closest[0], peer_po0);
    }

    #[test]
    fn test_topology_service_config_all_options() {
        // Test all TopologyServiceConfig options.
        let config = TopologyServiceConfig::new()
            .with_kademlia(
                KademliaConfig::default()
                    .with_low_watermark(2)
                    .with_high_watermark(16)
                    .with_saturation_peers(4),
            )
            .with_dial_interval(std::time::Duration::from_secs(10))
            .with_gossip_enabled();

        assert_eq!(config.dial_interval, Some(std::time::Duration::from_secs(10)));
        assert!(config.gossip.is_some());
        assert_eq!(config.kademlia.low_watermark, 2);
        assert_eq!(config.kademlia.high_watermark, 16);
    }

    #[test]
    fn test_topology_service_config_with_custom_gossip() {
        use crate::gossip::HiveGossipConfig;

        let gossip_config = HiveGossipConfig::default()
            .with_refresh_interval(std::time::Duration::from_secs(300))
            .with_max_peers_for_distant(8);

        let config = TopologyServiceConfig::new().with_gossip(gossip_config);

        assert!(config.gossip.is_some());
        let gossip = config.gossip.unwrap();
        assert_eq!(gossip.refresh_interval, std::time::Duration::from_secs(300));
        assert_eq!(gossip.max_peers_for_distant, 8);
    }

    #[tokio::test]
    async fn test_service_debug_impl_no_internal_leak() {
        // Verifies Debug impl doesn't expose internal Arc addresses or sensitive data.
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (service, _handle, _components) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        let debug_str = format!("{:?}", service);

        // Debug should show useful info but not Arc pointers
        assert!(debug_str.contains("TopologyService"));
        assert!(debug_str.contains("depth"));
        assert!(debug_str.contains("connected_peers"));
        // Should not contain raw pointer addresses from Arc
        assert!(!debug_str.contains("0x")); // Typical pointer format
    }

    #[tokio::test]
    async fn test_handle_debug_impl_no_internal_leak() {
        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);
        let (_executor, _manager) = test_executor();

        let (_service, handle, _components) =
            TopologyService::new(identity, &TestNetworkConfig::default(), TopologyServiceConfig::default())
                .expect("topology service creation should succeed");

        let debug_str = format!("{:?}", handle);

        assert!(debug_str.contains("TopologyHandle"));
        assert!(debug_str.contains("depth"));
        assert!(!debug_str.contains("0x"));
    }
}
