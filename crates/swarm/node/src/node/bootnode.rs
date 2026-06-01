//! Bootnode - minimal Swarm node with topology protocols only.
//!
//! A [`BootNode`] participates in peer discovery via handshake, hive, and pingpong
//! but does not run client protocols (pricing, retrieval, pushsync, settlement).
//!
//! Use this for dedicated bootnode servers that help new nodes join the network.

use std::collections::HashSet;

use eyre::Result;
use futures::StreamExt;
use libp2p::{PeerId, identity::PublicKey, swarm::NetworkBehaviour, swarm::SwarmEvent};
use metrics::{counter, gauge};
use nectar_primitives::SwarmAddress;
use tracing::{info, warn};
use vertex_swarm_api::{
    BandwidthMode, SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig,
};
use vertex_swarm_net_identify as identify;
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_topology::{
    ConnectionDirection, KademliaConfig, TopologyBehaviour, TopologyCommand, TopologyConfig,
    TopologyEvent, TopologyHandle,
};
use vertex_tasks::GracefulShutdown;

use super::base::BaseNode;
use super::builder::BuiltInfrastructure;

/// How a bootnode treats inbound hive peer gossip.
///
/// Mirrors bee's `BootnodeMode` (see
/// <https://github.com/ethersphere/bee/blob/master/pkg/hive/hive.go#L283-L284>):
/// bootnodes act as gossip amplifiers and intentionally do *not* learn from the
/// hive payloads they help relay.
///
/// Unit 7 introduces a generic `HivePeerHandler` trait at the hive layer; this
/// enum is the standalone scaffolding until that lands. Once Unit 7 ships, the
/// bootnode wires its `DiscardSilently` impl into [`BootnodeBehaviour::from_parts`]
/// and this enum is reconciled with the trait dispatch.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum HiveIngestionMode {
    /// Discard inbound hive peer entries (bootnode default).
    #[default]
    Discard,
    /// Learn from inbound hive peer entries (client/storer default).
    Learn,
}

impl HiveIngestionMode {
    /// `true` if the mode discards inbound hive entries without learning.
    pub fn is_discard(&self) -> bool {
        matches!(self, HiveIngestionMode::Discard)
    }
}

/// Result labels for `bootnode_inbound_handshakes_total`.
mod handshake_result {
    pub const SUCCESS: &str = "success";
    pub const REJECTED: &str = "rejected";
    pub const DISCONNECTED: &str = "disconnected";
}

fn record_inbound_handshake(result: &'static str) {
    counter!("bootnode_inbound_handshakes_total", "result" => result).increment(1);
}

/// Warn (do not error) if a bootnode is being configured with a bandwidth mode
/// other than [`BandwidthMode::None`] — bootnodes do not sell bandwidth.
pub fn warn_if_bandwidth_enabled(mode: BandwidthMode) {
    if mode.is_enabled() {
        warn!(
            ?mode,
            "Bootnode running with bandwidth accounting enabled; bootnodes do not sell bandwidth — \
             consider --bandwidth.mode=none"
        );
    }
}

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
        let agent_versions = topology.agent_versions();
        Self {
            // Send listen addresses (even private IPs) so bee's peerstore has entries.
            // waitPeerAddrs() returns immediately if len(addrs) > 0, regardless of
            // whether addresses match or are reachable. The handshake protocol uses
            // RemoteMultiaddr directly. Private IPs in gossip are harmless.
            identify: identify::Behaviour::new(
                identify::Config::new(local_public_key),
                agent_versions,
            ),
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
    hive_ingestion_mode: HiveIngestionMode,
    /// Overlays of peers that completed an *inbound* handshake. Used to scope
    /// `bootnode_inbound_handshakes_total{result="disconnected"}` to inbound
    /// connections (PeerDisconnected does not carry direction).
    inbound_overlays: HashSet<OverlayAddress>,
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

    /// Hive ingestion mode in effect for this bootnode.
    pub fn hive_ingestion_mode(&self) -> HiveIngestionMode {
        self.hive_ingestion_mode
    }

    pub fn topology_command(&mut self, command: TopologyCommand) {
        self.base.swarm.behaviour_mut().topology.on_command(command);
    }

    pub fn start_listening(&mut self) -> Result<()> {
        self.base.start_listening()?;
        // Initialise the gauge to 0; libp2p emits NewListenAddr asynchronously
        // once each listener actually binds. The run loop reconciles the gauge
        // with `swarm.listeners()` on every NewListenAddr / ExpiredListenAddr /
        // ListenerClosed event.
        gauge!("bootnode_listen_addrs").set(0.0);
        Ok(())
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
        info!(mode = ?self.hive_ingestion_mode, "Starting bootnode event loop");

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

    fn handle_topology_event(&mut self, event: TopologyEvent) {
        // Record inbound-handshake-result counters for bootnode visibility.
        // PeerDisconnected does not carry direction, so we track inbound
        // overlays from PeerReady to scope the disconnect counter correctly.
        match &event {
            TopologyEvent::PeerReady {
                overlay,
                direction: ConnectionDirection::Inbound,
                ..
            } => {
                self.inbound_overlays.insert(*overlay);
                record_inbound_handshake(handshake_result::SUCCESS);
            }
            TopologyEvent::PeerRejected {
                direction: ConnectionDirection::Inbound,
                ..
            } => {
                record_inbound_handshake(handshake_result::REJECTED);
            }
            TopologyEvent::PeerDisconnected { overlay, .. } => {
                if self.inbound_overlays.remove(overlay) {
                    record_inbound_handshake(handshake_result::DISCONNECTED);
                }
            }
            _ => {}
        }
    }

    fn handle_swarm_event(&mut self, event: SwarmEvent<BootnodeEvent>) {
        // Keep the listen-addrs gauge in sync with libp2p's view before delegating
        // to the common handler (which logs but doesn't mutate listen_addrs).
        match &event {
            SwarmEvent::NewListenAddr { .. }
            | SwarmEvent::ExpiredListenAddr { .. }
            | SwarmEvent::ListenerClosed { .. } => {
                gauge!("bootnode_listen_addrs").set(self.base.swarm.listeners().count() as f64);
            }
            _ => {}
        }

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
    hive_ingestion_mode: HiveIngestionMode,
    bandwidth_mode: Option<BandwidthMode>,
}

impl<I: SwarmIdentity + Clone> BootNodeBuilder<I> {
    pub fn new(identity: I) -> Self {
        Self {
            identity,
            infra: None,
            kademlia_config: None,
            hive_ingestion_mode: HiveIngestionMode::Discard,
            bandwidth_mode: None,
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

    /// Override hive ingestion mode (default: [`HiveIngestionMode::Discard`]).
    pub fn with_hive_ingestion_mode(mut self, mode: HiveIngestionMode) -> Self {
        self.hive_ingestion_mode = mode;
        self
    }

    /// Inform the builder of the operator-configured [`BandwidthMode`].
    ///
    /// At build time this triggers a warning if the mode is not
    /// [`BandwidthMode::None`] — bootnodes do not sell bandwidth.
    pub fn with_bandwidth_mode(mut self, mode: BandwidthMode) -> Self {
        self.bandwidth_mode = Some(mode);
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
        I: vertex_swarm_spec::HasSpec,
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        info!("Initializing bootnode P2P network...");

        if let Some(mode) = self.bandwidth_mode {
            warn_if_bandwidth_enabled(mode);
        }

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

        Ok(BootNode {
            base,
            hive_ingestion_mode: self.hive_ingestion_mode,
            inbound_overlays: HashSet::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hive_ingestion_mode_default_is_discard() {
        assert_eq!(HiveIngestionMode::default(), HiveIngestionMode::Discard);
        assert!(HiveIngestionMode::default().is_discard());
        assert!(!HiveIngestionMode::Learn.is_discard());
    }

    #[test]
    fn hive_ingestion_mode_label_is_snake_case() {
        let label: &'static str = HiveIngestionMode::Discard.into();
        assert_eq!(label, "discard");
        let label: &'static str = HiveIngestionMode::Learn.into();
        assert_eq!(label, "learn");
    }

    #[test]
    fn warn_if_bandwidth_enabled_does_not_panic() {
        warn_if_bandwidth_enabled(BandwidthMode::None);
        warn_if_bandwidth_enabled(BandwidthMode::Pseudosettle);
        warn_if_bandwidth_enabled(BandwidthMode::Swap);
        warn_if_bandwidth_enabled(BandwidthMode::Both);
    }

    /// All metrics named by Unit 12 must register without panicking on first emit.
    #[test]
    fn metric_helpers_register_without_panicking() {
        record_inbound_handshake(handshake_result::SUCCESS);
        record_inbound_handshake(handshake_result::REJECTED);
        record_inbound_handshake(handshake_result::DISCONNECTED);
        gauge!("bootnode_listen_addrs").set(0.0);

        vertex_swarm_net_hive::metrics::record_broadcast_sent();
        vertex_swarm_net_hive::metrics::record_peer_discarded(
            vertex_swarm_net_hive::metrics::DiscardReason::BootnodeMode,
        );
        vertex_swarm_net_hive::metrics::record_peer_discarded(
            vertex_swarm_net_hive::metrics::DiscardReason::Unreachable,
        );
        vertex_swarm_topology::metrics::record_kademlia_eviction(
            vertex_swarm_topology::metrics::EvictionPolicy::Bootnode,
        );
        vertex_swarm_topology::metrics::record_kademlia_eviction(
            vertex_swarm_topology::metrics::EvictionPolicy::Standard,
        );
    }
}
