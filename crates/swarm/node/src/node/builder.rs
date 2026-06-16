//! Shared builder infrastructure for node types.

use std::time::Duration;

use eyre::{Result, WrapErr};
use libp2p::{Multiaddr, Swarm, identity::PublicKey, swarm::NetworkBehaviour};
use tracing::{info, warn};
use vertex_net_peer_store::PeerSnapshotStore;
use vertex_swarm_api::{
    SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig, SwarmTopologyCommands,
};
use vertex_swarm_peer_manager::PeerSnapshot;
use vertex_swarm_spec::HasSpec;
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviour, TopologyBehaviourBuilder, TopologyConfig, TopologyHandle,
};

use super::base::BaseNode;
use super::error::NodeBuildError;

use crate::BootnodeProvider;

pub(crate) type PeerStore = std::sync::Arc<dyn PeerSnapshotStore<PeerSnapshot>>;

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

        let mut builder = TopologyBehaviourBuilder::new(identity.clone(), &config_with_bootnodes)
            .with_config(topology_config);

        // The live per-IP concurrent-connection cap is off by default
        // (`DEFAULT_MAX_CONNECTIONS_PER_IP = None`); no per-target override
        // is needed here.

        if let Some(store) = peer_store {
            builder = builder.with_peer_store(store);
        }
        let (mut topology_behaviour, topology_handle) = builder
            .try_build()
            .wrap_err("failed to create topology behaviour")?;
        topology_behaviour
            .spawn_tasks()
            .wrap_err("failed to spawn topology background tasks")?;

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

    fn autonat_enabled(&self) -> bool {
        self.inner.autonat_enabled()
    }

    fn upnp_enabled(&self) -> bool {
        self.inner.upnp_enabled()
    }

    fn mdns_enabled(&self) -> bool {
        self.inner.mdns_enabled()
    }

    fn trust_local_peers(&self) -> bool {
        self.inner.trust_local_peers()
    }

    fn connection_profile(&self) -> Option<vertex_swarm_api::ConnectionProfile> {
        self.inner.connection_profile()
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
/// topology so we can call `register_local_peer_id`.
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

    let behaviour_builder = |keypair: &libp2p::identity::Keypair| {
        let topology = topology_cell
            .lock()
            .map_err(|_| NodeBuildError::TopologyCellPoisoned)?
            .take()
            .ok_or(NodeBuildError::TopologyBehaviourTaken)?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(behaviour_fn(
            keypair.public().clone(),
            topology,
        ))
    };

    let swarm = build_swarm(idle_timeout, behaviour_builder)?;

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

/// Assemble the libp2p [`Swarm`] for native targets over a TCP transport with
/// DNS resolution, Noise authentication, and Yamux multiplexing.
#[cfg(not(target_arch = "wasm32"))]
fn build_swarm<B, F>(idle_timeout: Duration, behaviour_builder: F) -> Result<Swarm<B>>
where
    B: NetworkBehaviour,
    F: FnOnce(
        &libp2p::identity::Keypair,
    ) -> std::result::Result<B, Box<dyn std::error::Error + Send + Sync>>,
{
    use libp2p::{SwarmBuilder, noise, tcp, yamux};

    let swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_dns()?
        .with_behaviour(behaviour_builder)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(idle_timeout))
        .build();

    Ok(swarm)
}

/// Assemble the libp2p [`Swarm`] for the browser over a websocket transport.
///
/// The browser cannot open listeners, so this path only dials. It builds the
/// websocket transport from `libp2p-websocket-websys`, which dials the live
/// AutoTLS `/tls/sni/<host>/ws` multiaddrs (the browser performs TLS and SNI
/// natively), then upgrades it with Noise and Yamux to match the native
/// authentication and multiplexing.
///
/// The upgrade negotiates with [`Version::V1Lazy`] rather than [`Version::V1`].
/// The browser `WebSocket` is message-framed and its `AsyncWrite` reports a
/// flush as complete only once the socket's buffered amount drains, which it
/// observes through a periodic timer rather than synchronously. Under the
/// browser executor the strict-`V1` flush-then-await round trip leaves the
/// security negotiation parked mid-handshake, and the connection is torn down
/// before the peer's protocol confirmation is read. The dialer offers a single
/// security (`/noise`) and a single muxer (`/yamux/1.0.0`), so the lazy variant
/// settles on each optimistically and folds the confirmation into the first
/// read of the following handshake bytes. The responder still replies with a
/// regular `V1` response, so this stays wire-compatible; it only removes the
/// synchronous flush barrier the browser transport cannot satisfy.
#[cfg(target_arch = "wasm32")]
fn build_swarm<B, F>(idle_timeout: Duration, behaviour_builder: F) -> Result<Swarm<B>>
where
    B: NetworkBehaviour,
    F: FnOnce(
        &libp2p::identity::Keypair,
    ) -> std::result::Result<B, Box<dyn std::error::Error + Send + Sync>>,
{
    use libp2p::{SwarmBuilder, Transport as _, core::upgrade::Version, noise, yamux};

    let swarm = SwarmBuilder::with_new_identity()
        .with_wasm_bindgen()
        .with_other_transport(|keypair| {
            let transport = libp2p_websocket_websys::Transport::default()
                .upgrade(Version::V1Lazy)
                .authenticate(noise::Config::new(keypair)?)
                .multiplex(yamux::Config::default());
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(transport)
        })?
        .with_behaviour(behaviour_builder)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(idle_timeout))
        .build();

    Ok(swarm)
}
