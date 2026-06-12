//! Browser client launch entrypoint.
//!
//! The native launch path (`vertex-swarm-builder`) pulls the redb database, the
//! chain provider, the SWAP settlement service, and the gRPC server, none of
//! which build for `wasm32`. A browser client needs none of them: it mints an
//! ephemeral identity, dials the live mainnet bootnodes over secure websockets,
//! and watches its Kademlia topology fill. This module is the narrow,
//! wasm-buildable assembly that does exactly that.
//!
//! [`launch_client`] composes a [`ClientNode`] from an identity, a resolved
//! bootnode set, and the browser-friendly [`WasmClientConfig`], spawns the node
//! run loop and the peer-manager tick on the wasm executor, and returns the
//! [`TopologyHandle`] so a UI can poll readiness and stream topology events. The
//! returned handle keeps the client alive for the session; dropping it does not
//! stop the spawned tasks, which run until the page is torn down.

use std::time::Duration;

use eyre::{Result, WrapErr};
use libp2p::Multiaddr;
use vertex_swarm_api::{
    DefaultPeerConfig, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_peer_manager::{DEFAULT_TICK_INTERVAL, spawn_peer_manager_task};
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};
use vertex_tasks::TaskExecutor;

use super::client::ClientNode;

/// Connection idle timeout for the browser client.
const WASM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Transport-layer cap on established connections for the browser client.
///
/// A saturated routing table sits comfortably below this; it is a resource
/// backstop, not a topology knob.
const WASM_MAX_PEERS: usize = 400;

/// Network configuration for a browser client node.
///
/// The browser cannot open listeners and has no LAN to discover peers on, so
/// this configuration carries no listen addresses and leaves mDNS, UPnP, and
/// AutoNAT off. It holds only the resolved bootnode multiaddrs (browser-dialable
/// `/tls/sni/<host>/ws` leaves) plus the Kademlia routing defaults. It is the
/// wasm counterpart to the CLI-driven `NetworkConfig` the native builder uses,
/// trimmed to what a dial-only client needs.
pub struct WasmClientConfig {
    bootnodes: Vec<Multiaddr>,
    peer: DefaultPeerConfig,
    routing: KademliaConfig,
}

impl WasmClientConfig {
    /// Build a browser client configuration from resolved bootnode multiaddrs.
    ///
    /// `bootnodes` are the browser-dialable leaves from the DoH resolver or the
    /// embedded snapshot. Routing uses the Kademlia defaults.
    #[must_use]
    pub fn new(bootnodes: Vec<Multiaddr>) -> Self {
        Self {
            bootnodes,
            peer: DefaultPeerConfig::default(),
            routing: KademliaConfig::default(),
        }
    }
}

impl SwarmNetworkConfig for WasmClientConfig {
    fn listen_addrs(&self) -> &[Multiaddr] {
        &[]
    }

    fn bootnodes(&self) -> &[Multiaddr] {
        &self.bootnodes
    }

    fn discovery_enabled(&self) -> bool {
        true
    }

    fn max_peers(&self) -> usize {
        WASM_MAX_PEERS
    }

    fn idle_timeout(&self) -> Duration {
        WASM_IDLE_TIMEOUT
    }

    fn nat_auto_enabled(&self) -> bool {
        false
    }

    fn autonat_enabled(&self) -> bool {
        false
    }

    fn upnp_enabled(&self) -> bool {
        false
    }

    fn mdns_enabled(&self) -> bool {
        false
    }
}

impl SwarmPeerConfig for WasmClientConfig {
    type Peers = DefaultPeerConfig;

    fn peers(&self) -> &Self::Peers {
        &self.peer
    }
}

impl SwarmRoutingConfig for WasmClientConfig {
    type Routing = KademliaConfig;

    fn routing(&self) -> &Self::Routing {
        &self.routing
    }
}

/// Build and start a browser client node, returning its topology handle.
///
/// Assembles a [`ClientNode`] over the wasm websocket transport from the given
/// ephemeral `identity` and resolved `bootnodes`, spawns the node run loop and
/// the peer-manager tick on the current wasm [`TaskExecutor`], and hands back the
/// [`TopologyHandle`] the caller polls for readiness and subscribes to for
/// [`TopologyEvent`](vertex_swarm_topology::TopologyEvent)s. The node dials its
/// bootnodes as part of startup; from there the Kademlia routing table fills on
/// its own.
///
/// The returned handle owns nothing the spawned tasks need, so the client keeps
/// running after the handle is dropped. The browser tears the session down by
/// dropping the whole wasm instance.
///
/// # Errors
///
/// Returns an error if the swarm fails to assemble (transport or behaviour
/// construction) or the node fails to begin listening.
pub async fn launch_client(
    identity: Identity,
    bootnodes: Vec<Multiaddr>,
) -> Result<TopologyHandle<Identity>> {
    let config = WasmClientConfig::new(bootnodes);

    let (node, client_service, _client_handle) = ClientNode::builder(identity)
        .build(&config, None)
        .await
        .wrap_err("failed to build browser client node")?;

    let topology = node.topology_handle().clone();

    let executor = TaskExecutor::current();
    // The peer manager tick is pure data and `Send`, so it goes through the
    // ordinary spawner.
    spawn_peer_manager_task(
        topology.peer_manager().clone(),
        DEFAULT_TICK_INTERVAL,
        &executor,
    );

    // The client service drives the retrieval and pushsync request paths. The
    // demo does not issue downloads, but the service must run so the node can
    // answer the protocols its peers speak during topology building. Its task is
    // `Send`, so it goes through the ordinary service spawner.
    executor.spawn_service("swarm.client_service", client_service);

    // The node run loop owns the libp2p swarm, whose websocket transport futures
    // are `!Send`. It starts listening (a no-op for the browser, which cannot
    // listen) and then dials bootnodes and services the event loop for the
    // session, so it spawns on the browser-local path too.
    executor.spawn_local_with_graceful_shutdown_signal(
        "swarm.client_node",
        move |shutdown| async move {
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "browser client node exited with error");
            }
        },
    );

    Ok(topology)
}
