//! Shared builder infrastructure for node types.

use std::{sync::Arc, time::Duration};

use eyre::Result;
use libp2p::Multiaddr;
use tracing::info;
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmNodeTypes, SwarmTopology};
use vertex_swarm_kademlia::{KademliaConfig, KademliaTopology};
use vertex_swarm_peermanager::{
    AddressManager, DiscoverySender, PeerManager, PeerStore, discovery_channel,
    run_peer_store_consumer,
};
use vertex_swarm_topology::BootnodeConnector;
use vertex_tasks::TaskExecutor;

use crate::BootnodeProvider;

/// Common builder configuration for all node types.
#[derive(Clone)]
pub struct BuilderConfig<N: SwarmNodeTypes> {
    pub identity: N::Identity,
    pub listen_addrs: Vec<Multiaddr>,
    pub bootnodes: Vec<Multiaddr>,
    pub idle_timeout: Duration,
    pub kademlia_config: KademliaConfig,
    pub peer_store: Option<Arc<dyn PeerStore>>,
    pub nat_addrs: Vec<Multiaddr>,
    pub nat_auto: bool,
}

impl<N: SwarmNodeTypes> BuilderConfig<N> {
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

    pub fn apply_network_config(&mut self, config: &impl SwarmNetworkConfig) {
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
    }
}

/// Pre-built infrastructure components ready for swarm assembly.
pub struct BuiltInfrastructure<N: SwarmNodeTypes> {
    pub identity: N::Identity,
    pub peer_manager: Arc<PeerManager>,
    pub address_manager: Option<Arc<AddressManager>>,
    pub kademlia: Arc<KademliaTopology<N::Identity>>,
    pub bootnode_connector: BootnodeConnector,
    pub listen_addrs: Vec<Multiaddr>,
    pub idle_timeout: Duration,
    pub discovery_tx: DiscoverySender,
}

impl<N: SwarmNodeTypes> BuiltInfrastructure<N> {
    pub fn from_config(config: BuilderConfig<N>) -> Result<Self> {
        let address_manager = {
            let mgr = AddressManager::new(config.nat_addrs.clone(), config.nat_auto);
            if !config.nat_addrs.is_empty() {
                info!(count = config.nat_addrs.len(), "NAT addresses configured");
                for addr in &config.nat_addrs {
                    info!(%addr, "NAT address");
                }
            }
            if config.nat_auto {
                info!("Auto NAT discovery enabled");
            }
            Some(Arc::new(mgr))
        };

        let peer_manager = match config.peer_store {
            Some(store) => {
                let pm = PeerManager::with_store(store)
                    .map_err(|e| eyre::eyre!("failed to initialize peer manager: {}", e))?;
                info!(count = pm.stats().stored_peers, "loaded peers from store");
                Arc::new(pm)
            }
            None => Arc::new(PeerManager::new()),
        };

        let kademlia = KademliaTopology::new(config.identity.clone(), config.kademlia_config);

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

        let bootnode_connector = BootnodeConnector::new(config.bootnodes);

        Ok(Self {
            identity: config.identity,
            peer_manager,
            address_manager,
            kademlia,
            bootnode_connector,
            listen_addrs: config.listen_addrs,
            idle_timeout: config.idle_timeout,
            discovery_tx,
        })
    }
}
