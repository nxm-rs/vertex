//! Bootnode - minimal Swarm node with topology protocols and pricing stub.
//!
//! A [`BootNode`] participates in peer discovery via handshake, hive, and
//! pingpong and advertises the pricing protocol in listen-only mode so peers
//! that disconnect on a failed pricing handshake stay connected. It does not
//! run the other client protocols (retrieval, pushsync, settlement).
//!
//! Use this for dedicated bootnode servers that help new nodes join the network.

use eyre::Result;
use futures::StreamExt;
use libp2p::{PeerId, identity::PublicKey, swarm::NetworkBehaviour, swarm::SwarmEvent};
use nectar_primitives::SwarmAddress;
use tracing::{debug, info};
use vertex_swarm_api::{
    SwarmIdentity, SwarmIdentityConfig, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig,
};
use vertex_swarm_net_identify as identify;
use vertex_swarm_net_pricing::{PricingBehaviour, PricingEvent};
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviour, TopologyCommand, TopologyConfig, TopologyEvent,
    TopologyHandle,
};
use vertex_tasks::GracefulShutdown;

use super::base::BaseNode;
use super::builder::BuiltInfrastructure;

/// Network behaviour for a bootnode (topology + pricing stub).
///
/// The pricing stub is mandatory for interop with peers that close the
/// connection when their pricing handshake fails; advertising the protocol
/// is enough. Bootnodes never dial an outbound pricing stream of their own
/// because they have no payment state to announce.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BootnodeEvent")]
pub struct BootnodeBehaviour<I: SwarmIdentity + Clone> {
    pub identify: identify::Behaviour,
    pub topology: TopologyBehaviour<I>,
    pub pricing: PricingBehaviour,
}

impl<I: SwarmIdentity + Clone> BootnodeBehaviour<I> {
    /// Create behaviour from pre-built topology (used with libp2p SwarmBuilder).
    pub fn from_parts(local_public_key: PublicKey, topology: TopologyBehaviour<I>) -> Self {
        let agent_versions = topology.agent_versions();
        Self {
            // Advertise listen addresses (even private IPs). Peers gate on
            // "we have at least one addr" rather than reachability, and the
            // handshake itself uses the libp2p-observed remote multiaddr, so
            // private IPs in the identify payload are harmless.
            identify: identify::Behaviour::new(
                identify::Config::new(local_public_key),
                agent_versions,
            ),
            topology,
            // Bootnodes advertise pricing for interop but never dial an
            // outbound stream of their own and discard inbound thresholds.
            pricing: PricingBehaviour::new_bootnode(),
        }
    }

    /// Protocols advertised by this behaviour above the topology layer.
    ///
    /// Used in tests and diagnostics to verify mainnet interop coverage.
    /// Topology protocols (handshake, hive, pingpong) are owned by
    /// [`TopologyBehaviour`] and tested there; this list covers only the
    /// extra protocols that the bootnode itself mounts (currently the
    /// pricing stub required for bee mainnet interop).
    #[allow(dead_code)] // Reserved for integration tests and operator diagnostics.
    pub fn supported_protocols() -> &'static [&'static str] {
        &[vertex_swarm_net_pricing::PROTOCOL_NAME]
    }
}

/// Events from the bootnode behaviour.
#[allow(clippy::large_enum_variant)]
pub enum BootnodeEvent {
    Identify(Box<identify::Event>),
    Topology(()),
    Pricing(PricingEvent),
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

impl From<PricingEvent> for BootnodeEvent {
    fn from(event: PricingEvent) -> Self {
        BootnodeEvent::Pricing(event)
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
                    self.base.swarm.behaviour_mut().topology.on_command(TopologyCommand::SavePeers);
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
            BootnodeEvent::Pricing(event) => {
                // Bootnodes do not perform accounting; observability only.
                match event {
                    PricingEvent::ThresholdReceived { peer, threshold } => {
                        debug!(
                            %peer,
                            threshold = %threshold.payment_threshold,
                            "Bootnode: received pricing threshold (discarded)"
                        );
                    }
                    PricingEvent::InboundError { peer, error } => {
                        debug!(%peer, %error, "Bootnode: pricing inbound error");
                    }
                    // Variants associated with announcing our own threshold
                    // cannot fire in listen-only mode.
                    PricingEvent::AnnouncementSent { .. }
                    | PricingEvent::OutboundError { .. }
                    | PricingEvent::AnnouncementDropped { .. } => {}
                    _ => {}
                }
            }
        }
    }

    fn handle_identify_event(&mut self, event: identify::Event) {
        let behaviour = self.base.swarm.behaviour_mut();
        super::base::handle_identify_event(&behaviour.topology, &mut behaviour.identify, event);
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
                        Score = vertex_swarm_peer_score::PeerScore,
                        Error = vertex_net_peer_store::error::StoreError,
                    >,
            >,
        >,
    ) -> Result<BootNode<I>>
    where
        I: vertex_swarm_spec::HasSpec + SwarmIdentityConfig,
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        // Bootnodes have a well-known overlay address that peers rely on
        // across restarts. Reject ephemeral identities before doing any
        // network setup so misconfiguration fails fast.
        self.identity
            .assert_persistent_identity(vertex_swarm_primitives::SwarmNodeType::Bootnode)?;

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

        let base = super::builder::build_base_node(
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

        Ok(BootNode { base })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use libp2p::Multiaddr;
    use std::sync::Arc;
    use std::time::Duration;
    use vertex_swarm_api::{DefaultPeerConfig, IdentityError, SwarmIdentityConfig};
    use vertex_swarm_identity::Identity;
    use vertex_swarm_primitives::SwarmNodeType;

    /// Minimal config used purely to exercise the bootnode build trait bounds.
    /// We never reach the network-init code path: `assert_persistent_identity`
    /// must fail before any peer/routing config is touched.
    struct TestConfig {
        peers: DefaultPeerConfig,
        routing: KademliaConfig,
        empty_addrs: Vec<Multiaddr>,
    }

    impl TestConfig {
        fn new() -> Self {
            Self {
                peers: DefaultPeerConfig::default(),
                routing: KademliaConfig::default(),
                empty_addrs: Vec::new(),
            }
        }
    }

    impl SwarmNetworkConfig for TestConfig {
        fn listen_addrs(&self) -> &[Multiaddr] {
            &self.empty_addrs
        }
        fn bootnodes(&self) -> &[Multiaddr] {
            &self.empty_addrs
        }
        fn trusted_peers(&self) -> &[Multiaddr] {
            &self.empty_addrs
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

    /// An ephemeral bootnode must be rejected at build time. The well-known
    /// overlay address contract requires a keystore-backed signing key.
    #[tokio::test]
    async fn ephemeral_bootnode_is_rejected_at_build() {
        let spec = vertex_swarm_spec::init_testnet();
        let identity = Arc::new(Identity::random(spec, SwarmNodeType::Bootnode));
        assert!(identity.ephemeral(), "test precondition");

        let network = TestConfig::new();

        let result = BootNode::builder(identity)
            .build(&network, None, None)
            .await;

        let err = match result {
            Ok(_) => panic!("ephemeral bootnode must fail to build"),
            Err(e) => e,
        };
        let identity_err = err
            .downcast_ref::<IdentityError>()
            .expect("error chain should carry an IdentityError");
        assert!(
            matches!(
                identity_err,
                IdentityError::EphemeralWhenPersistent {
                    node_type: SwarmNodeType::Bootnode
                }
            ),
            "expected EphemeralWhenPersistent {{ Bootnode }}, got: {identity_err:?}"
        );
    }

    /// The bootnode must advertise the pricing protocol so peers that
    /// disconnect on a failed pricing handshake stay connected.
    #[test]
    fn supported_protocols_includes_pricing() {
        let protocols = BootnodeBehaviour::<Arc<Identity>>::supported_protocols();
        assert!(
            protocols.contains(&vertex_swarm_net_pricing::PROTOCOL_NAME),
            "bootnode must advertise pricing protocol; got {protocols:?}"
        );
    }
}
