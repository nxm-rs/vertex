//! Builder for [`TopologyBehaviour`], separating construction from task spawning.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};

use libp2p::Multiaddr;
use tokio::sync::{broadcast, mpsc};
use tracing::info;
use vertex_net_dialer::{DialTracker, DialTrackerConfig};
use vertex_net_local::LocalCapabilities;
use vertex_swarm_api::{PeerConfigValues, SwarmBootnodeConfig, SwarmIdentity};
use vertex_swarm_net_handshake::HANDSHAKE_TIMEOUT;
use vertex_swarm_net_identify as identify;
use vertex_swarm_peer_manager::{PeerManager, PeerManagerConfig};
use vertex_swarm_peer_score::SwarmScoringConfig;
use vertex_swarm_spec::{HasSpec, Spec};

use crate::behaviour::{
    COMMAND_CHANNEL_CAPACITY, ConnectionRegistry, EVENT_CHANNEL_CAPACITY, LazyInterval, PeerStore,
    TopologyBehaviour, TopologyConfig,
};
use crate::composed::ProtocolBehaviours;
use crate::error::TopologyError;
use crate::gossip::{GossipChannels, GossipConfig, gossip_channel, spawn_gossip_task};
use crate::handle::TopologyHandle;
use crate::kademlia::{
    KademliaRouting, RoutingEvaluatorHandle, kademlia_admission_control, spawn_evaluator,
};
use crate::metrics::TopologyMetrics;
use crate::nat_discovery::LocalAddressManager;

/// Inputs the background tasks need, captured at build time so that
/// [`TopologyBehaviour::spawn_tasks`] can start them later without re-deriving
/// state from the behaviour.
pub(crate) struct PendingTopologyTasks {
    /// Network spec for the gossip task's ephemeral verifier identity.
    spec: Arc<Spec>,
    /// Tuning knobs for the gossip task and its verifier.
    gossip_config: GossipConfig,
    /// Shared local capability tracker, also held by the address manager.
    local_capabilities: Arc<LocalCapabilities>,
    /// Task-side gossip channel endpoints.
    gossip_channels: GossipChannels,
}

/// Builder for [`TopologyBehaviour`] and its [`TopologyHandle`].
///
/// Captures the values it needs from the network configuration at
/// construction, takes the optional stores through fluent `with_*` methods,
/// and constructs the behaviour in [`Self::try_build`] without spawning any
/// background tasks. Spawning is a separate, explicit step
/// ([`TopologyBehaviour::spawn_tasks`]), so unit tests can construct a
/// behaviour without a tokio runtime.
pub struct TopologyBehaviourBuilder<I: SwarmIdentity + Clone> {
    identity: I,
    config: TopologyConfig,
    bootnodes: Vec<Multiaddr>,
    trusted_peers: Vec<Multiaddr>,
    nat_addrs: Vec<Multiaddr>,
    trust_local_peers: bool,
    scoring_config: SwarmScoringConfig,
    max_per_bin: usize,
    peer_store: Option<PeerStore>,
}

impl<I: SwarmIdentity + Clone> TopologyBehaviourBuilder<I> {
    /// Create a builder from the node identity and network configuration.
    ///
    /// Copies the multiaddrs and peer-management values out of
    /// `network_config` so the builder owns everything it needs.
    pub fn new(identity: I, network_config: &impl SwarmBootnodeConfig) -> Self {
        let peer_config = network_config.peers();
        Self {
            identity,
            config: TopologyConfig::default(),
            bootnodes: network_config.bootnodes().to_vec(),
            trusted_peers: network_config.trusted_peers().to_vec(),
            nat_addrs: network_config.nat_addrs().to_vec(),
            trust_local_peers: network_config.trust_local_peers(),
            scoring_config: SwarmScoringConfig::builder()
                .ban_threshold(peer_config.ban_threshold())
                .warn_threshold(peer_config.warn_threshold())
                .build(),
            max_per_bin: peer_config.max_per_bin(),
            peer_store: None,
        }
    }

    /// Set the topology configuration (kademlia, dial cadence, save interval).
    pub fn with_config(mut self, config: TopologyConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the peer snapshot store.
    ///
    /// Without one the node runs ephemeral: peers learned in this session are
    /// lost on shutdown. With one, the peer set is loaded at startup and
    /// snapshotted periodically and on shutdown.
    pub fn with_peer_store(mut self, store: PeerStore) -> Self {
        self.peer_store = Some(store);
        self
    }

    /// Build the behaviour and its handle without spawning background tasks.
    ///
    /// The returned behaviour carries the captured task inputs; call
    /// [`TopologyBehaviour::spawn_tasks`] from within a tokio runtime to start
    /// the connection evaluator, interface watcher, and gossip tasks.
    /// Construction itself needs no runtime.
    pub fn try_build(self) -> Result<(TopologyBehaviour<I>, TopologyHandle<I>), TopologyError>
    where
        I: HasSpec,
    {
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);

        let peer_manager = PeerManager::new(
            &self.identity,
            PeerManagerConfig {
                scoring: self.scoring_config,
                max_per_bin: self.max_per_bin,
                store: self.peer_store,
                ..Default::default()
            },
        );

        let connection_registry = Arc::new(ConnectionRegistry::new());
        let agent_versions = identify::new_agent_versions();

        let lifecycle_rx = peer_manager.subscribe();

        let routing = KademliaRouting::new(
            self.identity.clone(),
            self.config.kademlia.clone(),
            peer_manager.clone(),
        );

        let local_capabilities = Arc::new(LocalCapabilities::new());

        // LocalAddressManager handles NAT address advertisement
        // Note: We no longer track peer-observed addresses - they contain
        // ephemeral NAT ports that only work for the specific peer connection.
        let nat_discovery = Arc::new(if !self.nat_addrs.is_empty() {
            info!(count = self.nat_addrs.len(), "NAT addresses configured");
            LocalAddressManager::new(local_capabilities.clone(), self.nat_addrs)
        } else {
            LocalAddressManager::disabled(local_capabilities.clone())
        });

        let spec = <I as HasSpec>::spec(&self.identity).clone();
        let identity = Arc::new(self.identity);

        // Wire kademlia routing as the handshake admission gate so the
        // routing layer can veto a peer before the local side commits
        // to its final exchange message.
        let admission_control = kademlia_admission_control(routing.clone());

        // Create composed protocol behaviours
        let protocols =
            ProtocolBehaviours::new(identity.clone(), nat_discovery.clone(), admission_control);

        let metrics = Arc::new(TopologyMetrics::new());

        let handle = TopologyHandle::new(
            identity.clone(),
            routing.clone(),
            connection_registry.clone(),
            peer_manager.clone(),
            command_tx,
            event_tx.clone(),
            agent_versions.clone(),
            metrics.clone(),
        );

        // Queue static NAT addresses to emit as external addresses on first poll
        let pending_nat_external_addrs = nat_discovery.nat_addrs().to_vec();

        // Handles wired here; the tasks behind them start in `spawn_tasks`.
        let evaluator_handle = RoutingEvaluatorHandle::new();
        let (gossip, gossip_channels) = gossip_channel();
        let gossip_config = self.config.gossip.clone();

        let behaviour = TopologyBehaviour {
            identity,
            protocols,
            routing,
            peer_manager,
            connection_registry,
            nat_discovery,
            bootnodes: self.bootnodes,
            trusted_peers: self.trusted_peers,
            command_rx,
            event_tx,
            pending_actions: VecDeque::new(),
            gossip,
            dial_interval: LazyInterval::new(self.config.dial_interval),
            pending_bootnode_resolution: None,
            evaluator_handle,
            dial_tracker: DialTracker::new(DialTrackerConfig {
                max_pending: 0,     // not used as a queue, only for direct in-flight tracking
                max_in_flight: 256, // generous limit; routing capacity is the real gate
                pending_ttl: HANDSHAKE_TIMEOUT,
                in_flight_timeout: HANDSHAKE_TIMEOUT,
                cleanup_interval: Duration::from_secs(30),
                metrics_label: Some("topology"),
                ..Default::default()
            }),
            early_disconnect_threshold: self.config.early_disconnect_threshold,
            pending_evictions: HashSet::new(),
            outbound_public_dials: HashSet::new(),
            connection_remote_ips: HashMap::new(),
            lifecycle_rx,
            agent_versions,
            trust_local_peers: self.trust_local_peers,
            pending_nat_external_addrs,
            metrics,
            pending_tasks: Some(PendingTopologyTasks {
                spec,
                gossip_config,
                local_capabilities,
                gossip_channels,
            }),
        };

        Ok((behaviour, handle))
    }
}

impl<I: SwarmIdentity + Clone + 'static> TopologyBehaviour<I> {
    /// Spawn the background tasks that drive this behaviour: the kademlia
    /// connection evaluator, the network interface watcher, and the gossip
    /// task (peer exchange and verification).
    ///
    /// Requires a [`vertex_tasks::TaskExecutor`] reachable through
    /// [`vertex_tasks::TaskExecutor::try_current`]. The captured task inputs
    /// are consumed on the first successful call; a second call returns
    /// [`TopologyError::TasksAlreadySpawned`]. If no executor is available the
    /// inputs are preserved so the call can be retried.
    pub fn spawn_tasks(&mut self) -> Result<(), TopologyError> {
        if self.pending_tasks.is_none() {
            return Err(TopologyError::TasksAlreadySpawned);
        }
        let executor = vertex_tasks::TaskExecutor::try_current()
            .map_err(|e| TopologyError::TaskSpawn(e.to_string()))?;
        let Some(pending) = self.pending_tasks.take() else {
            return Err(TopologyError::TasksAlreadySpawned);
        };

        // Spawn background connection evaluator
        spawn_evaluator(self.routing.clone(), &self.evaluator_handle, &executor);

        // Spawn interface watcher for push-based subnet discovery.
        crate::tasks::spawn_interface_watcher(&executor);

        // Spawn the gossip task (merged peer exchange + verification).
        spawn_gossip_task(
            pending.spec,
            pending.gossip_config,
            self.identity.overlay_address(),
            self.peer_manager.clone(),
            self.connection_registry.clone(),
            self.evaluator_handle.clone(),
            pending.local_capabilities,
            pending.gossip_channels,
            &executor,
        )
        .map_err(|e| TopologyError::TaskSpawn(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use vertex_swarm_api::{
        DefaultPeerConfig, SwarmNetworkConfig, SwarmNodeType, SwarmPeerConfig, SwarmRoutingConfig,
    };
    use vertex_swarm_identity::Identity;

    use crate::kademlia::KademliaConfig;

    /// Minimal network configuration for exercising the builder.
    struct TestConfig {
        peers: DefaultPeerConfig,
        routing: KademliaConfig,
        bootnodes: Vec<Multiaddr>,
        empty_addrs: Vec<Multiaddr>,
    }

    impl TestConfig {
        fn new() -> Self {
            Self {
                peers: DefaultPeerConfig::default(),
                routing: KademliaConfig::default(),
                bootnodes: vec!["/ip4/127.0.0.1/tcp/1634".parse().expect("valid multiaddr")],
                empty_addrs: Vec::new(),
            }
        }
    }

    impl SwarmNetworkConfig for TestConfig {
        fn listen_addrs(&self) -> &[Multiaddr] {
            &self.empty_addrs
        }
        fn bootnodes(&self) -> &[Multiaddr] {
            &self.bootnodes
        }
        fn discovery_enabled(&self) -> bool {
            true
        }
        fn max_peers(&self) -> usize {
            32
        }
        fn idle_timeout(&self) -> Duration {
            Duration::from_secs(60)
        }
    }

    impl SwarmPeerConfig for TestConfig {
        type Peers = DefaultPeerConfig;
        fn peers(&self) -> &Self::Peers {
            &self.peers
        }
    }

    impl SwarmRoutingConfig for TestConfig {
        type Routing = KademliaConfig;
        fn routing(&self) -> &Self::Routing {
            &self.routing
        }
    }

    fn test_identity() -> Identity {
        Identity::random(vertex_swarm_spec::init_testnet(), SwarmNodeType::Client)
    }

    /// The builder constructs a behaviour and handle without a tokio runtime
    /// and without spawning any background tasks.
    #[test]
    fn try_build_without_runtime_does_not_spawn_tasks() {
        let config = TestConfig::new();
        let (behaviour, _handle) = TopologyBehaviourBuilder::new(test_identity(), &config)
            .with_config(TopologyConfig::default().with_dial_interval(Duration::from_secs(7)))
            .try_build()
            .expect("build without runtime");

        assert!(
            behaviour.pending_tasks.is_some(),
            "task inputs must be captured, not spawned, at build time"
        );
        assert_eq!(behaviour.bootnodes, config.bootnodes);
        assert!(behaviour.trust_local_peers);
    }

    /// Without a global task executor, spawning fails and preserves the
    /// captured inputs so the call can be retried.
    #[test]
    fn spawn_tasks_without_executor_is_retryable() {
        let config = TestConfig::new();
        let (mut behaviour, _handle) = TopologyBehaviourBuilder::new(test_identity(), &config)
            .try_build()
            .expect("build without runtime");

        let err = behaviour
            .spawn_tasks()
            .expect_err("no task executor in unit tests");
        assert!(matches!(err, TopologyError::TaskSpawn(_)));
        assert!(
            behaviour.pending_tasks.is_some(),
            "missing executor must not consume the task inputs"
        );
    }

    /// A second spawn attempt reports the tasks as already spawned.
    #[test]
    fn spawn_tasks_twice_reports_already_spawned() {
        let config = TestConfig::new();
        let (mut behaviour, _handle) = TopologyBehaviourBuilder::new(test_identity(), &config)
            .try_build()
            .expect("build without runtime");

        // Simulate a successful first spawn.
        behaviour.pending_tasks = None;
        assert!(matches!(
            behaviour.spawn_tasks(),
            Err(TopologyError::TasksAlreadySpawned)
        ));
    }
}
