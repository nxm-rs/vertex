//! Bootnode - minimal Swarm node with topology protocols and a listen-only
//! pricing handler.
//!
//! A [`BootNode`] participates in peer discovery via handshake, hive, and
//! ping and advertises the pricing protocol in listen-only mode so peers
//! that disconnect on a failed pricing handshake stay connected. It does not
//! initiate any pricing announcement of its own and does not run the other
//! client protocols (retrieval, pushsync, pseudosettle).

use std::convert::Infallible;

use eyre::Result;
use futures::StreamExt;
use libp2p::connection_limits;
use libp2p::{PeerId, identity::PublicKey, swarm::NetworkBehaviour, swarm::SwarmEvent};
use nectar_primitives::SwarmAddress;
use tracing::info;
use vertex_swarm_api::{
    SwarmIdentity, SwarmIdentityConfig, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig,
};
use vertex_swarm_net_identify as identify;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviour, TopologyCommand, TopologyConfig, TopologyEvent,
    TopologyHandle,
};
use vertex_tasks::GracefulShutdown;

use super::base::BaseNode;
use super::builder::BuiltInfrastructure;
use super::nat::{NatBehaviour, NatEvent};
use crate::protocol::{BehaviourConfig as ClientBehaviourConfig, ClientBehaviour, ClientEvent};

/// Network behaviour for a bootnode (topology + listen-only client behaviour).
///
/// The same client behaviour composed into [`ClientNode`](super::ClientNode) is
/// used here with [`SwarmNodeType::Bootnode`], which narrows the advertised
/// client protocol set to pricing only. The bootnode never issues client
/// commands (`AnnouncePricing`, `RetrieveChunk`, ...), so only the inbound
/// pricing path is ever exercised.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BootnodeEvent")]
pub(crate) struct BootnodeBehaviour<I: SwarmIdentity + Clone> {
    /// Transport-level connection caps (total, per-peer, pending). Listed
    /// first so a denied connection is rejected before the other behaviours
    /// allocate per-connection state.
    pub(crate) connection_limits: connection_limits::Behaviour,
    pub(crate) identify: identify::Behaviour,
    /// NAT traversal (AutoNAT v2, UPnP) and LAN discovery (mDNS), composed as
    /// one sub-behaviour.
    pub(crate) nat: NatBehaviour,
    pub(crate) topology: TopologyBehaviour<I>,
    pub(crate) client: ClientBehaviour,
}

impl<I: SwarmIdentity + Clone> BootnodeBehaviour<I> {
    /// Create behaviour from pre-built topology (used with libp2p SwarmBuilder).
    pub fn from_parts(
        local_public_key: PublicKey,
        topology: TopologyBehaviour<I>,
        nat: NatBehaviour,
        connection_limits: connection_limits::Behaviour,
    ) -> Self {
        let agent_versions = topology.agent_versions();
        Self {
            connection_limits,
            // Identify advertises addresses scoped to each peer (see
            // `addresses_for_remote`): a public peer never receives our private
            // or loopback addresses, matching the handshake's policy.
            identify: identify::Behaviour::new(
                identify::Config::new(local_public_key),
                agent_versions,
            ),
            nat,
            topology,
            client: ClientBehaviour::new(ClientBehaviourConfig::for_role(SwarmNodeType::Bootnode)),
        }
    }

    /// Wire-protocol identifiers advertised by this behaviour above the
    /// topology layer. Topology's protocols (handshake, hive, ping) are
    /// owned by `topology` itself and are not enumerated here. Currently
    /// only the test below consumes this; the `dead_code` allow keeps the
    /// helper available for future diagnostics and operator-facing readouts.
    #[allow(dead_code)]
    pub fn supported_protocols() -> &'static [&'static str] {
        &[vertex_swarm_net_pricing::PROTOCOL_NAME]
    }
}

/// Events from the bootnode behaviour.
#[allow(clippy::large_enum_variant)]
pub enum BootnodeEvent {
    Identify(Box<identify::Event>),
    Nat(NatEvent),
    Topology(()),
    Client(ClientEvent),
}

impl From<Infallible> for BootnodeEvent {
    fn from(event: Infallible) -> Self {
        // The connection-limits behaviour never emits events.
        match event {}
    }
}

impl From<identify::Event> for BootnodeEvent {
    fn from(event: identify::Event) -> Self {
        BootnodeEvent::Identify(Box::new(event))
    }
}

impl From<NatEvent> for BootnodeEvent {
    fn from(event: NatEvent) -> Self {
        BootnodeEvent::Nat(event)
    }
}

impl From<()> for BootnodeEvent {
    fn from(_: ()) -> Self {
        BootnodeEvent::Topology(())
    }
}

impl From<ClientEvent> for BootnodeEvent {
    fn from(event: ClientEvent) -> Self {
        BootnodeEvent::Client(event)
    }
}

/// A minimal Swarm node with only topology protocols.
///
/// Unlike [`ClientNode`](super::ClientNode), this excludes all client protocols
/// (pricing, retrieval, pushsync, settlement). Bootnodes only participate in
/// peer discovery via handshake, hive, and ping.
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
            BootnodeEvent::Nat(event) => {
                let local_peer_id = *self.base.swarm.local_peer_id();
                super::nat::handle_nat_event(
                    local_peer_id,
                    &mut self.base.swarm.behaviour_mut().topology,
                    event,
                );
            }
            BootnodeEvent::Topology(_) => {}
            BootnodeEvent::Client(event) => {
                // Bootnodes only accept pricing-receive; observability only.
                tracing::debug!(?event, "bootnode client event");
            }
        }
    }

    fn handle_identify_event(&mut self, event: identify::Event) {
        super::base::handle_identify_event(&mut self.base.swarm.behaviour_mut().identify, event);
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
        peer_store: Option<super::builder::PeerStore>,
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
                )?
            }
        };

        let connection_limits = super::base::build_connection_limits(network_config);
        let base = super::builder::build_base_node(
            infra,
            network_config,
            "Bootnode",
            move |pk, topology| {
                let nat = NatBehaviour::from_config(network_config, pk.to_peer_id());
                BootnodeBehaviour::from_parts(pk, topology, nat, connection_limits)
            },
        )
        .await?;

        // Register the local PeerId for address advertisement in handshakes
        base.swarm
            .behaviour()
            .topology
            .register_local_peer_id(*base.swarm.local_peer_id());

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

    /// The bootnode advertises the pricing protocol so peers that disconnect
    /// on a failed pricing handshake stay connected.
    #[test]
    fn bootnode_advertises_pricing_in_supported_protocols() {
        let protocols = BootnodeBehaviour::<Arc<Identity>>::supported_protocols();
        assert!(
            protocols.contains(&vertex_swarm_net_pricing::PROTOCOL_NAME),
            "bootnode must advertise pricing protocol; got {protocols:?}"
        );
    }

    /// An ephemeral bootnode must be rejected at build time. The well-known
    /// overlay address contract requires a keystore-backed signing key.
    #[tokio::test]
    async fn ephemeral_bootnode_is_rejected_at_build() {
        let spec = vertex_swarm_spec::init_testnet();
        let identity = Arc::new(Identity::random(spec, SwarmNodeType::Bootnode));
        assert!(identity.ephemeral(), "test precondition");

        let network = TestConfig::new();

        let result = BootNode::builder(identity).build(&network, None).await;

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
}
