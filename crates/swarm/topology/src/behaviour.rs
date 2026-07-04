//! Network topology behaviour managing peer connections via handshake, hive, and ping.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::StreamExt;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandlerInEvent,
        THandlerOutEvent, ToSwarm,
    },
};
use tracing::{debug, info, warn};
use vertex_metrics::TimingSampler;
use vertex_net_local::{AddressScope, classify_multiaddr, same_subnet};
use vertex_net_peer_store::PeerSnapshotStore;
use vertex_net_ratelimiter::{Quota, RateLimitedErr, RateLimiter};
use vertex_swarm_api::{
    BanCause, ConnectionProfile, DisconnectReason, PeerLifecycleEvent, SwarmIdentity,
};
use vertex_swarm_net_hive::MAX_BATCH_SIZE;
use vertex_swarm_net_identify as identify;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_manager::{PeerManager, PeerSnapshot, TrustLevel};
use vertex_swarm_primitives::{Bin, NeighborhoodDepth, OverlayAddress, all_bins};

use crate::DialReason;
use crate::dial_state::{DialBinState, DialState};
use vertex_net_dialer::DialTracker;
use vertex_net_peer_registry::{ActivePeers, PeerRegistry};

pub(crate) type ConnectionRegistry = PeerRegistry<OverlayAddress, Option<DialReason>>;

/// Boxed future that resolves `/dnsaddr/` bootnodes and trusted peers into
/// dialable multiaddrs (resolved bootnodes, resolved trusted).
///
/// The `Send` bound follows the platform executor via
/// [`vertex_tasks::MaybeSendBoxFuture`]: `Send` on native, unbounded on wasm32
/// where DNS-over-HTTPS resolution is backed by `fetch`, whose future is
/// `!Send`, and the swarm is single-threaded.
pub(crate) type BootnodeResolutionFuture =
    vertex_tasks::MaybeSendBoxFuture<(Vec<Multiaddr>, Vec<Multiaddr>)>;
use crate::TopologyCommand;
use crate::builder::PendingTopologyTasks;
use crate::composed::ProtocolBehaviours;
use crate::events::TopologyEvent;
use crate::extract_peer_id;
use crate::gossip::{GossipConfig, GossipEngine};
use crate::kademlia::{KademliaConfig, KademliaRouting, RoutingEvaluatorHandle, SwarmRouting};
use crate::metrics::{POLL_EVENTS_TOTAL, TopologyMetrics, po_label};
use crate::nat_discovery::LocalAddressManager;

/// Type-erased peer snapshot store.
pub(crate) type PeerStore = Arc<dyn PeerSnapshotStore<PeerSnapshot>>;

/// Post-handshake connections shorter than this are penalized as early
/// disconnects, so a peer that repeatedly connects and immediately leaves is
/// scored down.
const DEFAULT_EARLY_DISCONNECT_THRESHOLD: Duration = Duration::from_secs(30);

/// Event broadcast buffer (256 allows burst without blocking poll loop).
pub(crate) const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Command buffer (64 is sufficient for typical dial/disconnect rate).
pub(crate) const COMMAND_CHANNEL_CAPACITY: usize = 64;

/// Target for dialing a peer (internal).
///
/// `DialTarget` is only ever passed by value to `Self::dial(...)` and dropped
/// at the end of that call; it never lives in a collection. The size
/// asymmetry between `Known` and `Unknown` is a one-shot stack cost per
/// dial, so boxing the `SwarmPeer` would just add a heap allocation for no
/// real benefit.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum DialTarget {
    /// Known peer from gossip/store - has overlay for verification during handshake.
    Known(SwarmPeer),

    /// Unknown peer - overlay learned at handshake.
    /// Multiaddr must contain /p2p/ component.
    Unknown(Multiaddr),
}

impl DialTarget {
    /// Get the PeerId for this dial target, if present in the multiaddr.
    pub(crate) fn peer_id(&self) -> Option<PeerId> {
        match self {
            Self::Known(peer) => peer.multiaddrs().iter().find_map(extract_peer_id),
            Self::Unknown(addr) => extract_peer_id(addr),
        }
    }

    /// Get the overlay address if known.
    pub(crate) fn overlay(&self) -> Option<OverlayAddress> {
        match self {
            Self::Known(peer) => Some(OverlayAddress::from(*peer.overlay())),
            Self::Unknown(_) => None,
        }
    }

    /// Get the addresses to dial.
    pub(crate) fn addrs(&self) -> Vec<Multiaddr> {
        match self {
            Self::Known(peer) => peer.multiaddrs().to_vec(),
            Self::Unknown(addr) => vec![addr.clone()],
        }
    }
}

/// Configuration for topology behaviour.
///
/// Pacing (evaluation cadence, dial-rate quota, dial concurrency, bootstrap
/// fill, candidate budgets) is resolved from a [`ConnectionProfile`] at build
/// time: the explicit [`Self::with_connection_profile`] selection wins,
/// otherwise the network configuration's choice, otherwise the node-type
/// default. The `Option` overrides here ([`Self::with_dial_interval`],
/// [`Self::with_dial_quota`]) pin single knobs over whatever the profile
/// resolves to, for tests and embedders.
#[derive(Debug, Clone)]
pub struct TopologyConfig {
    pub kademlia: KademliaConfig,
    /// Tuning knobs for gossip peer exchange and record intake.
    pub gossip: GossipConfig,
    /// Explicit pacing profile; `None` defers to the network configuration
    /// and then the node-type default.
    pub connection_profile: Option<ConnectionProfile>,
    /// Explicit connection-evaluation cadence; `None` uses the profile's
    /// evaluation interval.
    pub dial_interval: Option<Duration>,
    /// Explicit discovery dial-rate quota; `None` uses the profile's quota.
    pub dial_quota: Option<Quota>,
    pub early_disconnect_threshold: Duration,
}

impl Default for TopologyConfig {
    fn default() -> Self {
        Self {
            kademlia: KademliaConfig::default(),
            gossip: GossipConfig::default(),
            connection_profile: None,
            dial_interval: None,
            dial_quota: None,
            early_disconnect_threshold: DEFAULT_EARLY_DISCONNECT_THRESHOLD,
        }
    }
}

impl TopologyConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_kademlia(mut self, config: KademliaConfig) -> Self {
        self.kademlia = config;
        self
    }

    /// Override the gossip tuning knobs.
    pub fn with_gossip(mut self, config: GossipConfig) -> Self {
        self.gossip = config;
        self
    }

    /// Select the connection pacing profile, overriding the network
    /// configuration and the node-type default.
    pub fn with_connection_profile(mut self, profile: ConnectionProfile) -> Self {
        self.connection_profile = Some(profile);
        self
    }

    /// Pin the connection-evaluation cadence over the profile's value.
    pub fn with_dial_interval(mut self, interval: Duration) -> Self {
        self.dial_interval = Some(interval);
        self
    }

    /// Pin the discovery dial-rate quota over the profile's value.
    pub fn with_dial_quota(mut self, quota: Quota) -> Self {
        self.dial_quota = Some(quota);
        self
    }

    pub fn with_early_disconnect_threshold(mut self, threshold: Duration) -> Self {
        self.early_disconnect_threshold = threshold;
        self
    }
}

/// Network topology behaviour managing peer connections.
///
/// Creates and owns all internal state (routing, peer_manager, dial_tracker, etc.)
/// and provides a [`TopologyHandle`] for external queries and commands.
///
/// Composes `HandshakeBehaviour`, `HiveBehaviour`, and `libp2p::ping::Behaviour` for
/// protocol handling, delegating to each while coordinating connection state.
pub struct TopologyBehaviour<I: SwarmIdentity + Clone> {
    pub(crate) identity: Arc<I>,

    /// Composed protocol behaviours (handshake, hive, ping).
    pub(crate) protocols: ProtocolBehaviours<I>,

    // Shared with TopologyHandle (Arc for external access)
    pub(crate) routing: Arc<KademliaRouting<I>>,
    pub(crate) peer_manager: Arc<PeerManager<I>>,

    // Owned (internal only, Arc for handler sharing and routing integration)
    pub(crate) connection_registry: Arc<ConnectionRegistry>,
    pub(crate) nat_discovery: Arc<LocalAddressManager>,
    pub(crate) bootnodes: Vec<Multiaddr>,
    pub(crate) trusted_peers: Vec<Multiaddr>,

    // Channels
    pub(crate) command_rx: mpsc::Receiver<TopologyCommand>,
    pub(crate) event_tx: broadcast::Sender<TopologyEvent>,

    // Pending swarm actions (dials, close connections, external addrs)
    pub(crate) pending_actions: VecDeque<ToSwarm<(), THandlerInEvent<ProtocolBehaviours<I>>>>,

    // Gossip peer-exchange policy, driven by connection events and polled
    // for its timers alongside the dial interval.
    pub(crate) gossip: GossipEngine<I>,

    // Periodic dial interval
    pub(crate) dial_interval: vertex_tasks::time::Interval,

    /// GCRA bucket shaping the discovery dial rate. Bursts after a candidate
    /// influx drain immediately up to the bucket size; beyond it, candidates
    /// stay queued in routing until tokens replenish.
    pub(crate) dial_rate: RateLimiter,

    /// Armed when the dial-rate bucket refused a candidate: fires when the
    /// next token is available so the drain resumes without waiting for the
    /// evaluation tick.
    pub(crate) dial_rate_timer: Option<vertex_tasks::time::BoxTimerFuture>,

    // Pending dnsaddr resolution for bootnodes (resolved_bootnodes, resolved_trusted)
    pub(crate) pending_bootnode_resolution: Option<BootnodeResolutionFuture>,

    /// Isolation probe state: `(next_probe_at, current_delay)` while the
    /// table is drained (nothing connected or pending, no candidate queued),
    /// `None` otherwise. Backs off exponentially so a dead network is probed
    /// at a decaying rate instead of every tick.
    pub(crate) isolation_probe: Option<(vertex_tasks::time::Instant, Duration)>,

    /// Static NAT addresses to emit as external addresses on first poll.
    /// Cleared after emitting to avoid re-emission.
    pub(crate) pending_nat_external_addrs: Vec<Multiaddr>,

    /// Handle for triggering background connection evaluation.
    pub(crate) evaluator_handle: RoutingEvaluatorHandle,

    /// Dial tracker for all outbound dials.
    /// Overlay may be unknown at dial time (bootnodes, commands).
    pub(crate) dial_tracker: DialTracker<OverlayAddress, DialReason>,

    /// Threshold for detecting post-handshake early disconnects.
    pub(crate) early_disconnect_threshold: Duration,

    /// Close intent recorded at each close site, consumed by
    /// `handle_connection_closed` so a deliberate close is attributed to its
    /// real reason rather than re-derived from the libp2p cause. Keyed by
    /// `PeerId` because every local close is `CloseConnection::All`.
    pub(crate) pending_closes: HashMap<PeerId, DisconnectReason>,

    /// Connection IDs of outbound dials whose remote address was public-scope.
    /// On handshake completion these promote the peer to
    /// [`crate::PeerReachability::Reachable`] (we reached a dialable public
    /// address). Populated at `ConnectionEstablished`, consumed at handshake
    /// completion, and cleared at `ConnectionClosed`.
    pub(crate) outbound_public_dials: HashSet<ConnectionId>,

    /// Receiver for the peer lifecycle event stream from PeerManager,
    /// wrapped as a stream so its waker wakes the behaviour the moment an
    /// event lands (a ban must not wait for the dial tick).
    ///
    /// Topology is the action-executing subscriber: `DisconnectRequested`
    /// and `Banned` events close the peer's connection; the remaining
    /// events are observability-only and ignored here. On lag the banned
    /// set is reconciled so a dropped `Banned` event cannot strand a banned
    /// peer connected (see [`PeerManager::subscribe`]).
    pub(crate) lifecycle_rx: BroadcastStream<PeerLifecycleEvent>,

    /// Agent versions received via identify, shared with identify behaviour.
    pub(crate) agent_versions: identify::AgentVersions,

    /// When set, same-subnet / private-LAN peers are protected from
    /// capacity-driven bin trimming by ranking above remotes of equal
    /// reachability. Liveness demotion and bans stay authoritative.
    pub(crate) trust_local_peers: bool,

    // Metrics
    pub(crate) metrics: Arc<TopologyMetrics>,

    /// Samples per-poll timing so the histogram record and clock reads fire on
    /// a fixed fraction of polls; the skip path is a decrement and branch.
    pub(crate) poll_timer: TimingSampler,

    /// Background-task inputs captured by [`crate::TopologyBehaviourBuilder`]
    /// and consumed by [`TopologyBehaviour::spawn_tasks`]. `None` once the
    /// tasks are running.
    pub(crate) pending_tasks: Option<PendingTopologyTasks>,
}

impl<I: SwarmIdentity + Clone> TopologyBehaviour<I> {
    // Public methods

    /// Register the local PeerId for address advertisement in handshakes.
    ///
    /// Must be called after the libp2p Swarm is built. All multiaddrs
    /// advertised to peers will include `/p2p/{peer_id}`.
    pub fn register_local_peer_id(&self, peer_id: PeerId) {
        self.nat_discovery.register_local_peer_id(peer_id);
    }

    /// Record an observed address reported by a peer.
    ///
    /// This is typically called with the `observed_addr` from libp2p identify.
    /// If the address is public, it updates our NAT discovery state to enable
    /// connections to other public peers.
    pub fn on_observed_addr(&self, addr: &Multiaddr) {
        self.nat_discovery.on_observed_addr(addr);
    }

    /// Promote a peer to [`crate::PeerReachability::Reachable`] after our AutoNAT
    /// v2 server dialed it back successfully.
    ///
    /// Wired from the node layer that owns the `autonat::v2::server::Behaviour`:
    /// for each `autonat::v2::server::Event` with an `Ok` result, the node
    /// forwards the verified `client` peer here.
    pub fn on_autonat_peer_confirmed(&self, peer: PeerId) {
        self.nat_discovery.on_autonat_peer_confirmed(peer);
    }

    /// Shared per-peer reachability tracker; cheap to clone.
    pub fn reachability(&self) -> crate::ReachabilityTracker {
        self.nat_discovery.reachability()
    }

    /// Shared agent version map, populated by identify and read by topology handle.
    pub fn agent_versions(&self) -> identify::AgentVersions {
        Arc::clone(&self.agent_versions)
    }

    /// Shared read view of the handshake-complete overlay-to-PeerId map. The
    /// connection registry stays the single writer; consumers only look up.
    pub fn active_peers(&self) -> Arc<dyn ActivePeers<OverlayAddress>> {
        self.connection_registry.clone()
    }

    /// Shared topology metrics (atomic counters for connected peers).
    pub fn metrics(&self) -> Arc<TopologyMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Close every connection to a peer, recording why so the close handler
    /// attributes it correctly instead of re-deriving it from the libp2p cause.
    ///
    /// The single choke point for locally-initiated closes: routing every
    /// close through here keeps the intent map and the close attribution in
    /// step. The one exception is the duplicate-connection eviction, which
    /// closes a specific stale connection (`CloseConnection::One`) that the
    /// close handler short-circuits before attribution.
    pub(crate) fn close_peer(&mut self, peer_id: PeerId, reason: DisconnectReason) {
        self.pending_closes.insert(peer_id, reason);
        self.pending_actions.push_back(ToSwarm::CloseConnection {
            peer_id,
            connection: libp2p::swarm::CloseConnection::All,
        });
    }

    /// Handle a topology command (dial, close connection, etc.).
    pub fn on_command(&mut self, command: TopologyCommand) {
        match command {
            TopologyCommand::ConnectBootnodes => {
                self.connect_bootnodes();
            }
            TopologyCommand::Dial(addr) => {
                self.dial(DialTarget::Unknown(addr), DialReason::Command);
            }
            TopologyCommand::CloseConnection(overlay) => {
                let Some(peer_id) = self.connection_registry.resolve_peer_id(&overlay) else {
                    tracing::warn!(%overlay, "Cannot close connection: peer not found");
                    return;
                };
                debug!(%overlay, %peer_id, "Close connection command");
                self.close_peer(peer_id, DisconnectReason::Requested);
            }
            TopologyCommand::BanPeer { overlay, reason } => {
                self.peer_manager.ban(&overlay, BanCause::Requested, reason);
                SwarmRouting::remove_peer(&*self.routing, &overlay);
                if let Some(peer_id) = self.connection_registry.resolve_peer_id(&overlay) {
                    debug!(%overlay, %peer_id, "Disconnecting banned peer via command");
                    self.close_peer(peer_id, DisconnectReason::Banned);
                }
                debug!(%overlay, "Banned peer via command");
            }
            TopologyCommand::SavePeers => {
                self.save_peers();
            }
            TopologyCommand::TriggerEvaluation => {
                self.run_evaluation_round();
            }
            TopologyCommand::DialState { reply } => {
                // A dropped receiver means the querier gave up; the snapshot
                // is a pure read so discarding it is free.
                let _ = reply.send(self.dial_state());
            }
        }
    }

    // Peer store

    /// Write the full peer set to the snapshot store (no-op without one).
    pub(crate) fn save_peers(&self) {
        self.peer_manager.snapshot();
        debug!(
            peers = self.peer_manager.index().len(),
            "Saved peer snapshot"
        );
    }

    /// Broadcast every action the gossip engine returned, in the same call
    /// that produced it.
    pub(crate) fn apply_gossip_actions(&mut self, actions: Vec<crate::gossip::GossipAction>) {
        for action in actions {
            self.broadcast_peers(action.to, action.peers);
        }
    }

    pub(crate) fn broadcast_peers(&mut self, to: OverlayAddress, peers: Vec<SwarmPeer>) {
        let Some(state) = self.connection_registry.get(&to) else {
            tracing::warn!(%to, "Cannot broadcast: peer not found");
            return;
        };
        if let Some(connection_id) = state.connection_id() {
            let peer_id = state.peer_id();
            for chunk in peers.chunks(MAX_BATCH_SIZE) {
                self.protocols
                    .hive
                    .broadcast(peer_id, connection_id, chunk.to_vec());
            }
        }
    }

    // Routing

    /// Drain candidates from the background evaluator's per-bin queues and
    /// dial them, shaped by the dial-rate bucket.
    ///
    /// Each dialable candidate costs one token. When the bucket runs dry the
    /// candidate returns to its queue and a timer is armed for the bucket's
    /// reported wait, so the drain resumes as soon as a token replenishes
    /// instead of waiting for the next evaluation tick. Candidates that are
    /// no longer dialable (record gone, banned, in backoff, wrong address
    /// scope) are dropped without spending a token.
    pub(crate) fn drain_candidate_queues(&mut self, cx: &mut Context<'_>) {
        if let Some(timer) = self.dial_rate_timer.as_mut() {
            match timer.as_mut().poll(cx) {
                Poll::Ready(()) => self.dial_rate_timer = None,
                Poll::Pending => return,
            }
        }

        while let Some(overlay) = self.routing.pop_candidate() {
            let Some(swarm_peer) = self
                .peer_manager
                .get_dialable_peers(std::slice::from_ref(&overlay))
                .pop()
            else {
                continue;
            };
            if !self.can_advertise_to(&swarm_peer) {
                continue;
            }

            match self.dial_rate.try_consume() {
                Ok(()) => self.dial(DialTarget::Known(swarm_peer), DialReason::Discovery),
                Err(RateLimitedErr::TooSoon(wait)) => {
                    metrics::counter!("topology_dials_throttled_total").increment(1);
                    self.routing.requeue_candidate(overlay);
                    let mut timer: vertex_tasks::time::BoxTimerFuture =
                        Box::pin(vertex_tasks::time::sleep(wait));
                    if timer.as_mut().poll(cx).is_ready() {
                        // Sub-millisecond wait already elapsed; keep draining.
                        continue;
                    }
                    self.dial_rate_timer = Some(timer);
                    return;
                }
                Err(RateLimitedErr::TooLarge) => {
                    // Unreachable: a single token never exceeds the non-zero
                    // bucket size. Requeue and stop draining defensively.
                    self.routing.requeue_candidate(overlay);
                    return;
                }
            }
        }
    }

    /// Emit the consumer-facing side effects of a published depth change:
    /// refreshed bin-target gauges, the gossip depth broadcast, the
    /// [`TopologyEvent::DepthChanged`] event (which drives the readiness
    /// snapshots and metrics), and bin trimming on a raise.
    ///
    /// All of these observe the published (hysteresis-filtered) depth; a
    /// lowering held back by the stability window emits nothing until it is
    /// actually published.
    pub(crate) fn on_depth_changed(
        &mut self,
        old_depth: NeighborhoodDepth,
        new_depth: NeighborhoodDepth,
    ) {
        self.push_bin_targets();
        let actions = self.gossip.on_depth_changed(new_depth.get());
        self.apply_gossip_actions(actions);
        self.emit_event(TopologyEvent::DepthChanged {
            old_depth: old_depth.get(),
            new_depth: new_depth.get(),
        });
        if new_depth > old_depth {
            self.trim_overpopulated_bins();
        }
    }

    /// Run one connection-evaluation round: publish a pending depth lowering,
    /// notify the background evaluator, and probe for isolation.
    ///
    /// This is the tick body minus the stale-pending GC. The periodic tick
    /// and the forced-evaluation command both call it, so a forced round does
    /// exactly what a tick does. Notifying only queues candidates; every dial
    /// still pays a token in [`Self::drain_candidate_queues`].
    pub(crate) fn run_evaluation_round(&mut self) {
        // Publish a pending depth lowering whose stability window has
        // expired; connection events are the other publication path.
        self.refresh_published_depth();
        self.evaluator_handle.trigger_evaluation();
        // An all-backoff empty table is otherwise a fixed point: the
        // evaluator keeps producing empty candidate sets and no dial
        // ever fires again.
        self.reconnect_if_isolated();
    }

    /// Assemble a [`DialState`] snapshot from authoritative state in one poll
    /// pass, so the behaviour-owned dial tracker and dial-rate bucket are read
    /// consistently with the shared routing and peer-manager tables.
    pub(crate) fn dial_state(&self) -> DialState {
        let depth = self.routing.depth();
        let limits = self.routing.limits();

        let bins: Vec<DialBinState> = all_bins(self.routing.max_bin())
            .map(|bin| {
                let (connected, _known) = self.routing.bin_peer_counts(bin);
                let (dialing, _handshaking, _active) = self.routing.bin_phase_counts(bin);
                // Neighborhood bins connect to every peer and are not
                // slot-balanced, so report no slot coverage there.
                let slots_filled = if limits.target(bin, depth) == usize::MAX {
                    None
                } else {
                    Some(self.routing.filled_slots(bin))
                };
                DialBinState {
                    bin,
                    connected,
                    dialing,
                    outbound: self.routing.outbound_count(bin),
                    slots_filled,
                    queued: self.routing.queued_candidates_in_bin(bin),
                }
            })
            .collect();

        let available_dial_tokens = self.dial_rate.available_tokens();
        let next_dial_token_in = match self.dial_rate.try_peek() {
            Ok(()) => None,
            Err(RateLimitedErr::TooSoon(wait)) => Some(wait),
            Err(RateLimitedErr::TooLarge) => None,
        };

        DialState {
            in_flight_dials: self.dial_tracker.in_flight_count(),
            pending_dials: self.dial_tracker.pending_count(),
            queued_candidates: self.routing.queued_candidates_total(),
            eligible_known_peers: self.peer_manager.eligible_count(),
            peers_in_backoff: self.peer_manager.peers_in_backoff().len(),
            available_dial_tokens,
            next_dial_token_in,
            throttled: self.dial_rate.try_peek().is_err(),
            last_evaluation: self.routing.last_evaluation_elapsed(),
            min_outbound: limits.min_outbound(),
            bins,
        }
    }

    /// Re-run the depth hysteresis and emit the depth-change side effects
    /// if the published depth moved.
    ///
    /// Called from the periodic poll tick so a pending depth lowering
    /// publishes once its stability window expires even when no further
    /// connection events arrive.
    pub(crate) fn refresh_published_depth(&mut self) {
        let old_depth = self.routing.depth();
        self.routing.refresh_depth();
        let new_depth = self.routing.depth();
        if new_depth != old_depth {
            self.on_depth_changed(old_depth, new_depth);
        }
    }

    /// Evict surplus peers from overpopulated bins after depth change.
    ///
    /// Emits `CloseConnection` for each evicted peer. Existing event handlers
    /// (`handle_connection_closed`) handle cleanup of routing capacity and state.
    pub(crate) fn trim_overpopulated_bins(&mut self) {
        // Rank each candidate by reachability (the routing layer is overlay-keyed
        // and has no peer-id mapping; we own both the registry and the tracker).
        // When local-peer trust is on, a same-subnet / private-LAN peer ranks
        // above a remote peer of equal reachability: the tuple orders
        // lexicographically and the routing layer evicts the lowest rank first,
        // so `(reachability, is_local)` keeps locals last without ever
        // overriding a liveness demotion (a remote `Reachable` still outranks a
        // local `Unreachable`). With trust off, the locality bit is held at
        // `false` so ranking matches the reachability-only behaviour.
        //
        // Locality comes from the `TrustLevel` the peer manager stored at
        // handshake completion: one atomic load per candidate instead of
        // re-deriving address scope every trim round.
        let tracker = self.nat_discovery.reachability();
        let trust_local = self.trust_local_peers;
        let peer_manager = &self.peer_manager;
        let registry = &self.connection_registry;
        let candidates = self.routing.eviction_candidates(|overlay| {
            let reachability = registry
                .resolve_peer_id(overlay)
                .map(|peer_id| tracker.status(&peer_id))
                .unwrap_or(crate::PeerReachability::Unknown);
            let is_local = trust_local && peer_manager.trust_level(overlay) != TrustLevel::Normal;
            (reachability, is_local)
        });
        if candidates.is_empty() {
            return;
        }

        let mut trimmed = 0;
        for candidate in &candidates {
            // Explicitly configured peers are never evicted by trimming.
            if self.peer_manager.trust_level(&candidate.overlay) == TrustLevel::Trusted {
                continue;
            }

            let Some(peer_id) = self.connection_registry.resolve_peer_id(&candidate.overlay) else {
                continue;
            };

            debug!(
                %peer_id,
                overlay = %candidate.overlay,
                bin = candidate.bin.get(),
                phase = ?candidate.phase,
                "Evicting peer: bin overpopulated after depth change"
            );

            self.close_peer(peer_id, DisconnectReason::BinTrimmed);
            trimmed += 1;
        }

        if trimmed > 0 {
            info!(
                trimmed,
                total_candidates = candidates.len(),
                "Trimmed overpopulated bins"
            );
        }
    }

    // Peer lifecycle actions

    /// Execute the network-side consequence of a peer lifecycle event.
    ///
    /// `DisconnectRequested` closes the peer's connection; `Banned`
    /// additionally removes the peer from routing. All other events are
    /// observability-only here and remain available to any subscriber via
    /// the peer manager's lifecycle stream.
    pub(crate) fn on_lifecycle_event(&mut self, event: PeerLifecycleEvent) {
        match event {
            PeerLifecycleEvent::DisconnectRequested { overlay, reason } => {
                if let Some(peer_id) = self.connection_registry.resolve_peer_id(&overlay) {
                    debug!(%overlay, %peer_id, %reason, "Disconnecting peer on request");
                    self.close_peer(peer_id, reason);
                }
            }
            PeerLifecycleEvent::Banned { overlay, .. } => {
                SwarmRouting::remove_peer(&*self.routing, &overlay);
                if let Some(peer_id) = self.connection_registry.resolve_peer_id(&overlay) {
                    debug!(%overlay, %peer_id, "Disconnecting banned peer");
                    self.close_peer(peer_id, DisconnectReason::Banned);
                }
            }
            PeerLifecycleEvent::Connected { .. }
            | PeerLifecycleEvent::Disconnected { .. }
            | PeerLifecycleEvent::ScoreWarning { .. }
            | PeerLifecycleEvent::Unbanned { .. } => {}
        }
    }

    /// Close any still-connected banned peer.
    ///
    /// Called when the lifecycle receiver lags: dropped events may have
    /// included `Banned`, so the banned set is the source of truth to
    /// resynchronize against. A `DisconnectRequested` lost to lag is not
    /// replayed (see the lagged-receiver policy on `PeerManager::subscribe`).
    pub(crate) fn reconcile_banned_connections(&mut self) {
        for overlay in self.connection_registry.active_ids() {
            if !self.peer_manager.is_banned(&overlay) {
                continue;
            }
            SwarmRouting::remove_peer(&*self.routing, &overlay);
            if let Some(peer_id) = self.connection_registry.resolve_peer_id(&overlay) {
                debug!(%overlay, %peer_id, "Disconnecting banned peer found during reconciliation");
                self.close_peer(peer_id, DisconnectReason::Banned);
            }
        }
    }

    // Metrics and helpers

    pub(crate) fn emit_event(&self, event: TopologyEvent) {
        self.metrics.record_event(&event);
        let _ = self.event_tx.send(event);
    }

    /// Re-derive the topology phase after a connected-set or depth change
    /// and broadcast the transition when the phase moved. The periodic
    /// evaluator task covers the time-driven transitions between ticks.
    pub(crate) fn refresh_topology_phase(&self) {
        if let Some(transition) = self.routing.evaluate_phase() {
            self.emit_event(TopologyEvent::PhaseChanged {
                from: transition.from,
                to: transition.to,
                depth: transition.depth.get(),
            });
        }
    }

    /// Push routing gauges for a single bin and global totals.
    pub(crate) fn push_routing_gauges(&self, bin: Bin) {
        // The metric label key stays "po" (the established observability name).
        let label = po_label(bin.get());
        let (connected, known) = self.routing.bin_peer_counts(bin);
        let (dialing, handshaking, active) = self.routing.bin_phase_counts(bin);

        metrics::gauge!("topology_bin_connected_peers", "po" => label).set(connected as f64);
        metrics::gauge!("topology_bin_known_peers", "po" => label).set(known as f64);
        metrics::gauge!("topology_bin_dialing", "po" => label).set(dialing as f64);
        metrics::gauge!("topology_bin_handshaking", "po" => label).set(handshaking as f64);
        metrics::gauge!("topology_bin_active", "po" => label).set(active as f64);
        metrics::gauge!("topology_bin_effective", "po" => label)
            .set((dialing + handshaking + active) as f64);
    }

    /// Push per-bin target/ceiling gauges and the global nominal gauge (called on depth change).
    pub(crate) fn push_bin_targets(&self) {
        let depth = self.routing.depth();
        let limits = self.routing.limits();

        for bin in all_bins(self.routing.max_bin()) {
            let label = po_label(bin.get());
            let target = limits.target(bin, depth);
            let target_val = if target == usize::MAX {
                -1.0
            } else {
                target as f64
            };
            let ceiling_val = limits.ceiling(bin, depth);
            let ceiling = if ceiling_val == usize::MAX {
                -1.0
            } else {
                ceiling_val as f64
            };

            metrics::gauge!("topology_bin_target_peers", "po" => label).set(target_val);
            metrics::gauge!("topology_bin_ceiling_peers", "po" => label).set(ceiling);
        }

        metrics::gauge!("topology_bin_nominal_peers").set(limits.nominal() as f64);
    }

    /// The [`Bin`] a peer occupies in this node's table (its proximity order to
    /// the local overlay).
    pub(crate) fn bin_for(&self, peer: &OverlayAddress) -> Bin {
        Bin::from(self.identity.overlay_address().proximity(peer))
    }

    /// Check if we can advertise to a peer based on address scope.
    ///
    /// - Public peers: require public addresses (NAT or discovered)
    /// - Private peers on LAN: can use private addresses if on same subnet
    /// - Loopback peers: always dialable
    pub(crate) fn can_advertise_to(&self, peer: &SwarmPeer) -> bool {
        let peer_max_scope = peer.max_scope();

        match peer_max_scope {
            Some(AddressScope::Public) => {
                // Public peer - need public addresses
                self.nat_discovery.is_reachable()
            }
            Some(AddressScope::Private | AddressScope::LinkLocal) => {
                // Private/link-local peer - check if we share a subnet
                let listen_addrs = self.nat_discovery.listen_addrs();
                peer.multiaddrs().iter().any(|peer_addr| {
                    listen_addrs
                        .iter()
                        .any(|our_addr| same_subnet(our_addr, peer_addr))
                })
            }
            Some(AddressScope::Loopback) | None => {
                // Loopback or unknown - allow
                true
            }
        }
    }
}

/// Whether a peer is local: at least one of its multiaddrs is loopback,
/// link-local, or on a directly-connected subnet shared with one of our listen
/// addresses.
///
/// This is scope, not reachability. A same-subnet peer is
/// [`AddressScope::Private`] yet locally reachable; the two stay distinct so
/// trimming protection never masks a genuine liveness failure (tracked
/// separately as [`crate::PeerReachability::Unreachable`]).
pub(crate) fn peer_is_local(peer: &SwarmPeer, listen_addrs: &[Multiaddr]) -> bool {
    peer.multiaddrs()
        .iter()
        .any(|peer_addr| match classify_multiaddr(peer_addr) {
            Some(AddressScope::Loopback | AddressScope::LinkLocal) => true,
            Some(AddressScope::Private) => listen_addrs
                .iter()
                .any(|our_addr| same_subnet(our_addr, peer_addr)),
            // Public addresses (or addresses with no IP) are never local.
            Some(AddressScope::Public) | None => false,
        })
}

impl<I: SwarmIdentity + Clone + 'static> NetworkBehaviour for TopologyBehaviour<I> {
    type ConnectionHandler = <ProtocolBehaviours<I> as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = ();

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        self.protocols.handle_established_inbound_connection(
            connection_id,
            peer,
            local_addr,
            remote_addr,
        )
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
        port_use: PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        self.protocols.handle_established_outbound_connection(
            connection_id,
            peer,
            addr,
            role_override,
            port_use,
        )
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        // Forward swarm events to composed protocols
        self.protocols.on_swarm_event(event);

        match event {
            FromSwarm::ConnectionEstablished(established) => {
                self.handle_connection_established(established);
            }
            FromSwarm::ConnectionClosed(closed) => {
                self.handle_connection_closed(closed);
            }
            FromSwarm::DialFailure(failure) => {
                self.handle_dial_failure(failure);
            }
            FromSwarm::NewListenAddr(info) => {
                debug!(address = %info.addr, "New listen address");
                let capability_became_known =
                    self.nat_discovery.on_new_listen_addr(info.addr.clone());

                if capability_became_known {
                    debug!("Network capability now known, triggering immediate dial");
                    self.evaluator_handle.trigger_evaluation();
                    // Bootnodes and trusted peers are `DialTarget::Unknown` (no
                    // overlay), so the startup `connect_bootnodes()` dial is
                    // silently dropped while the dial capability is still
                    // `ip: None`: such a target never enters the scored
                    // candidate set, and `trigger_evaluation` above cannot
                    // recover it. Re-issue the connection attempt now that
                    // addresses pass the dial-eligibility filter.
                    // `connect_bootnodes()` is idempotent here: `dial()` skips
                    // already-tracked peers, and this runs exactly once per
                    // unknown->known transition because `on_new_listen_addr`
                    // returns `true` only on that edge.
                    self.connect_bootnodes();
                }
            }
            FromSwarm::ExpiredListenAddr(info) => {
                debug!(address = %info.addr, "Expired listen address");
                self.nat_discovery.on_expired_listen_addr(info.addr);
            }
            FromSwarm::ExternalAddrConfirmed(info) => {
                // Verified external address (AutoNAT v2 dial-back or UPnP map).
                // A confirmed public address flips public connectivity on.
                debug!(address = %info.addr, "External address confirmed");
                self.nat_discovery.on_external_addr_confirmed(info.addr);
            }
            FromSwarm::ExternalAddrExpired(info) => {
                // A verified external address lapsed (e.g. UPnP lease expiry).
                debug!(address = %info.addr, "External address expired");
                self.nat_discovery.on_external_addr_expired(info.addr);
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        // Forward to composed protocols
        self.protocols
            .on_connection_handler_event(peer_id, connection_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        // Sampled poll-duration timing; the guard lives to the end of poll on
        // sampled iterations and is None (no clock read) on skipped ones.
        let _poll_timer = self.poll_timer.start();

        // Emit any pending static NAT addresses as external addresses (one-time on startup)
        if !self.pending_nat_external_addrs.is_empty() {
            for addr in std::mem::take(&mut self.pending_nat_external_addrs) {
                debug!(addr = %addr, "Emitting static NAT address as external address");
                self.pending_actions
                    .push_back(ToSwarm::ExternalAddrConfirmed(addr));
            }
        }

        // Poll for commands from TopologyHandle
        while let Poll::Ready(Some(command)) = self.command_rx.poll_recv(cx) {
            self.on_command(command);
        }

        // Poll pending dnsaddr resolution for bootnodes
        if let Some(ref mut future) = self.pending_bootnode_resolution
            && let Poll::Ready((resolved_bootnodes, resolved_trusted)) = future.as_mut().poll(cx)
        {
            info!(
                bootnodes = resolved_bootnodes.len(),
                trusted = resolved_trusted.len(),
                "dnsaddr resolution complete, dialing bootnodes"
            );
            self.pending_bootnode_resolution = None;
            self.dial_bootnodes(resolved_bootnodes, resolved_trusted);
        }

        // Drive the gossip engine's timers (the neighbourhood refresh and due
        // deferred exchanges); its interval and deadline timer register their
        // own wakers.
        let depth = self.routing.depth().get();
        let actions = self.gossip.poll(cx, depth);
        self.apply_gossip_actions(actions);

        // Check for periodic dial candidate evaluation
        if self.dial_interval.poll_tick(cx).is_ready() {
            self.cleanup_stale_pending();
            self.run_evaluation_round();
        }

        // Poll composed protocols and process their events
        loop {
            match self.protocols.poll(cx) {
                Poll::Ready(ToSwarm::GenerateEvent(event)) => {
                    POLL_EVENTS_TOTAL.increment(1);
                    let (peer_id, connection_id) = event.peer_connection();
                    self.process_protocol_event(peer_id, connection_id, event);
                }
                Poll::Ready(ToSwarm::Dial { opts }) => {
                    return Poll::Ready(ToSwarm::Dial { opts });
                }
                Poll::Ready(ToSwarm::NotifyHandler {
                    peer_id,
                    handler,
                    event,
                }) => {
                    return Poll::Ready(ToSwarm::NotifyHandler {
                        peer_id,
                        handler,
                        event,
                    });
                }
                Poll::Ready(ToSwarm::CloseConnection {
                    peer_id,
                    connection,
                }) => {
                    return Poll::Ready(ToSwarm::CloseConnection {
                        peer_id,
                        connection,
                    });
                }
                Poll::Ready(ToSwarm::ExternalAddrConfirmed(addr)) => {
                    return Poll::Ready(ToSwarm::ExternalAddrConfirmed(addr));
                }
                Poll::Ready(ToSwarm::ExternalAddrExpired(addr)) => {
                    return Poll::Ready(ToSwarm::ExternalAddrExpired(addr));
                }
                Poll::Ready(ToSwarm::NewExternalAddrCandidate(addr)) => {
                    return Poll::Ready(ToSwarm::NewExternalAddrCandidate(addr));
                }
                Poll::Ready(ToSwarm::ListenOn { opts }) => {
                    return Poll::Ready(ToSwarm::ListenOn { opts });
                }
                Poll::Ready(ToSwarm::RemoveListener { id }) => {
                    return Poll::Ready(ToSwarm::RemoveListener { id });
                }
                Poll::Ready(ToSwarm::NewExternalAddrOfPeer { peer_id, address }) => {
                    return Poll::Ready(ToSwarm::NewExternalAddrOfPeer { peer_id, address });
                }
                Poll::Ready(_) => {}
                Poll::Pending => break,
            }
        }

        // Drain candidates produced by the background evaluator task, shaped
        // by the dial-rate bucket (arms a wake-up timer when throttled).
        self.drain_candidate_queues(cx);

        // Drain peer lifecycle events and execute the network-side actions
        // (disconnects and bans). Catches auto-bans from scoring events
        // processed above; the registered waker makes a new event wake this
        // poll so enforcement never waits for the dial tick.
        while let Poll::Ready(Some(item)) = self.lifecycle_rx.poll_next_unpin(cx) {
            match item {
                Ok(event) => self.on_lifecycle_event(event),
                Err(BroadcastStreamRecvError::Lagged(missed)) => {
                    // Dropped events may include Banned: resynchronize from
                    // the banned set so no banned peer stays connected.
                    warn!(
                        missed,
                        "peer lifecycle stream lagged; reconciling banned peers"
                    );
                    self.reconcile_banned_connections();
                }
            }
        }

        if let Some(action) = self.pending_actions.pop_front() {
            return Poll::Ready(action);
        }

        Poll::Pending
    }
}

impl<I: SwarmIdentity + Clone> Drop for TopologyBehaviour<I> {
    fn drop(&mut self) {
        // Final snapshot so shutdown state is not lost to the periodic
        // snapshot interval.
        self.peer_manager.snapshot();

        let active = self.connection_registry.active_count();
        let pending = self.connection_registry.pending_count();
        let depth = self.routing.depth();

        info!(
            active_peers = active,
            pending_connections = pending,
            depth = depth.get(),
            "Topology behaviour shutting down"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::kademlia::KademliaConfig;

    use alloy_primitives::{Address, B256, Signature};
    use nectar_primitives::SwarmAddress;
    use vertex_swarm_peer::Timestamp;
    use vertex_swarm_primitives::Nonce;

    fn peer_with_addr(addr: &str) -> SwarmPeer {
        SwarmPeer::from_parts(
            vec![addr.parse().expect("valid multiaddr")],
            Signature::test_signature(),
            SwarmAddress::from(B256::repeat_byte(1)),
            Nonce::ZERO,
            Timestamp::from_seconds(1),
            None,
            Address::ZERO,
        )
    }

    #[test]
    fn peer_is_local_link_local_and_loopback() {
        // Link-local and loopback are local without any listen-addr match; they
        // are deterministic regardless of host interfaces.
        let listen: Vec<Multiaddr> = Vec::new();
        assert!(peer_is_local(
            &peer_with_addr("/ip4/169.254.10.20/tcp/1634"),
            &listen
        ));
        assert!(peer_is_local(
            &peer_with_addr("/ip6/fe80::1/tcp/1634"),
            &listen
        ));
        assert!(peer_is_local(
            &peer_with_addr("/ip4/127.0.0.1/tcp/1634"),
            &listen
        ));
    }

    #[test]
    fn peer_is_local_public_is_not_local() {
        let listen = vec![
            "/ip4/192.168.1.5/tcp/1634"
                .parse::<Multiaddr>()
                .expect("valid"),
        ];
        // An off-subnet public address is never local, even with a private
        // listen address configured.
        assert!(!peer_is_local(
            &peer_with_addr("/ip4/8.8.8.8/tcp/1634"),
            &listen
        ));
    }

    #[test]
    fn peer_is_local_private_requires_shared_subnet() {
        // A private address with no matching listen subnet is not local: the
        // same_subnet check is against our own listen addresses, so an empty
        // listen set yields false for a private peer.
        let listen: Vec<Multiaddr> = Vec::new();
        assert!(!peer_is_local(
            &peer_with_addr("/ip4/10.1.2.3/tcp/1634"),
            &listen
        ));
    }

    #[test]
    fn test_topology_config() {
        let config = TopologyConfig::new()
            .with_kademlia(KademliaConfig::default().with_nominal(3))
            .with_dial_interval(Duration::from_secs(10))
            .with_connection_profile(ConnectionProfile::Conservative);

        assert_eq!(config.dial_interval, Some(Duration::from_secs(10)));
        assert_eq!(
            config.connection_profile,
            Some(ConnectionProfile::Conservative)
        );
        assert_eq!(config.kademlia.limits.nominal(), 3);

        // Unset pacing knobs stay unresolved until build time.
        let default = TopologyConfig::default();
        assert_eq!(default.connection_profile, None);
        assert_eq!(default.dial_interval, None);
        assert!(default.dial_quota.is_none());
    }

    use vertex_swarm_api::{
        DefaultPeerConfig, SwarmNetworkConfig, SwarmNodeType, SwarmPeerConfig, SwarmRoutingConfig,
    };
    use vertex_swarm_identity::Identity;

    use crate::TopologyBehaviourBuilder;

    /// Minimal network configuration for behaviour tests.
    struct EventTestConfig {
        peers: DefaultPeerConfig,
        routing: KademliaConfig,
        listen_addrs: Vec<Multiaddr>,
        empty_addrs: Vec<Multiaddr>,
    }

    impl EventTestConfig {
        fn new() -> Self {
            Self {
                peers: DefaultPeerConfig::default(),
                routing: KademliaConfig::default(),
                listen_addrs: Vec::new(),
                empty_addrs: Vec::new(),
            }
        }

        /// Report a configured listen address so the built behaviour is a
        /// listening node, not a dial-only one. A dial-only node pins its IP
        /// capability to dual-stack, which hides the unknown-capability window
        /// the bootnode-redial path exists to recover.
        fn listening() -> Self {
            Self {
                listen_addrs: vec!["/ip4/0.0.0.0/tcp/1634".parse().expect("valid")],
                ..Self::new()
            }
        }
    }

    impl SwarmNetworkConfig for EventTestConfig {
        fn listen_addrs(&self) -> &[Multiaddr] {
            &self.listen_addrs
        }
        fn bootnodes(&self) -> &[Multiaddr] {
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

    impl SwarmPeerConfig for EventTestConfig {
        type Peers = DefaultPeerConfig;
        fn peers(&self) -> &Self::Peers {
            &self.peers
        }
    }

    impl SwarmRoutingConfig for EventTestConfig {
        type Routing = KademliaConfig;
        fn routing(&self) -> &Self::Routing {
            &self.routing
        }
    }

    fn test_behaviour_with(config: TopologyConfig) -> TopologyBehaviour<Identity> {
        let identity = Identity::random(vertex_swarm_spec::init_testnet(), SwarmNodeType::Client);
        let (behaviour, _handle) = TopologyBehaviourBuilder::new(identity, &EventTestConfig::new())
            .with_config(config)
            .try_build()
            .expect("build without runtime");
        behaviour
    }

    fn test_behaviour() -> TopologyBehaviour<Identity> {
        test_behaviour_with(TopologyConfig::default())
    }

    /// A listening (non-dial-only) behaviour whose IP capability starts
    /// unknown until the first `NewListenAddr` arrives.
    fn test_behaviour_listening() -> TopologyBehaviour<Identity> {
        let identity = Identity::random(vertex_swarm_spec::init_testnet(), SwarmNodeType::Client);
        let (behaviour, _handle) =
            TopologyBehaviourBuilder::new(identity, &EventTestConfig::listening())
                .try_build()
                .expect("build without runtime");
        behaviour
    }

    async fn next_action(
        behaviour: &mut TopologyBehaviour<Identity>,
    ) -> ToSwarm<(), THandlerInEvent<ProtocolBehaviours<Identity>>> {
        tokio::time::timeout(
            Duration::from_secs(5),
            std::future::poll_fn(|cx| behaviour.poll(cx)),
        )
        .await
        .expect("behaviour must emit an action")
    }

    mod lifecycle {
        use super::*;

        use libp2p::swarm::ConnectionId;
        use vertex_net_peer_registry::{ActivePeers, ConnectionDirection};
        use vertex_swarm_api::BanCause;
        use vertex_swarm_net_handshake::{HandshakeError, HandshakeEvent};
        use vertex_swarm_peer_manager::LIFECYCLE_CHANNEL_CAPACITY;
        use vertex_swarm_test_utils::test_overlay;

        use crate::composed::ProtocolEvent;

        /// Register an active (handshake-complete) connection in the registry.
        fn activate_connection(
            behaviour: &TopologyBehaviour<Identity>,
            overlay: OverlayAddress,
        ) -> PeerId {
            let peer_id = PeerId::random();
            let connection_id = ConnectionId::new_unchecked(1);
            behaviour
                .connection_registry
                .connected_inbound(peer_id, connection_id);
            behaviour
                .connection_registry
                .activate(peer_id, connection_id, overlay);
            peer_id
        }

        /// A `Banned` lifecycle event drained in poll closes the peer's
        /// connection.
        #[tokio::test]
        async fn banned_peer_connection_closed_via_lifecycle_event() {
            let mut behaviour = test_behaviour();
            let overlay = test_overlay(1);
            let peer_id = activate_connection(&behaviour, overlay);

            behaviour
                .peer_manager
                .ban(&overlay, BanCause::Requested, Some("test".into()));

            match next_action(&mut behaviour).await {
                ToSwarm::CloseConnection {
                    peer_id: closed, ..
                } => assert_eq!(closed, peer_id),
                _ => panic!("expected CloseConnection for the banned peer"),
            }
        }

        /// A ban arriving while the behaviour is parked wakes the poll
        /// through the lifecycle stream's waker: enforcement must not wait
        /// for the dial tick (one hour here, so only the waker can resolve
        /// this before the timeout).
        #[tokio::test]
        async fn ban_wakes_a_parked_behaviour_without_a_tick() {
            let mut behaviour = test_behaviour_with(
                TopologyConfig::default().with_dial_interval(Duration::from_secs(3600)),
            );
            let overlay = test_overlay(1);
            let peer_id = activate_connection(&behaviour, overlay);

            let manager = Arc::clone(&behaviour.peer_manager);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                manager.ban(&overlay, BanCause::Requested, None);
            });

            let action = tokio::time::timeout(
                Duration::from_secs(2),
                std::future::poll_fn(|cx| behaviour.poll(cx)),
            )
            .await
            .expect("the ban must wake the parked behaviour");
            match action {
                ToSwarm::CloseConnection {
                    peer_id: closed, ..
                } => assert_eq!(closed, peer_id),
                other => panic!("expected CloseConnection, got {other:?}"),
            }
        }

        /// When the lifecycle receiver lags past a `Banned` event, the poll
        /// loop reconciles against the banned set so the peer still gets
        /// disconnected (the documented lagged-receiver policy).
        #[tokio::test]
        async fn lagged_lifecycle_stream_reconciles_banned_peers() {
            let mut behaviour = test_behaviour();
            let overlay = test_overlay(1);
            let peer_id = activate_connection(&behaviour, overlay);

            behaviour
                .peer_manager
                .ban(&overlay, BanCause::Requested, None);

            // Flood the channel so the Banned event is dropped before the
            // behaviour drains its receiver.
            let other = test_overlay(2);
            for _ in 0..(2 * LIFECYCLE_CHANNEL_CAPACITY) {
                behaviour
                    .peer_manager
                    .on_peer_disconnected(&other, DisconnectReason::RemoteClose);
            }

            match next_action(&mut behaviour).await {
                ToSwarm::CloseConnection {
                    peer_id: closed, ..
                } => assert_eq!(closed, peer_id),
                _ => panic!("expected CloseConnection from banned-set reconciliation"),
            }
        }

        /// A `DisconnectRequested` lifecycle event closes the connection.
        #[tokio::test]
        async fn disconnect_requested_closes_connection() {
            let mut behaviour = test_behaviour();
            let overlay = test_overlay(1);
            let peer_id = activate_connection(&behaviour, overlay);

            behaviour.on_lifecycle_event(PeerLifecycleEvent::DisconnectRequested {
                overlay,
                reason: DisconnectReason::LowScore,
            });

            match next_action(&mut behaviour).await {
                ToSwarm::CloseConnection {
                    peer_id: closed, ..
                } => assert_eq!(closed, peer_id),
                _ => panic!("expected CloseConnection for the disconnect request"),
            }
        }

        /// Build a handshake-failed protocol event for a given connection.
        fn failed_event(peer_id: PeerId, connection_id: ConnectionId) -> ProtocolEvent {
            ProtocolEvent::Handshake(HandshakeEvent::Failed {
                peer_id,
                connection_id,
                direction: ConnectionDirection::Inbound,
                // Timeout is not the peer's fault, so no reachability or score hit.
                error: HandshakeError::Timeout,
            })
        }

        /// A failed handshake on a duplicate connection must not remove the
        /// healthy connection's Active entry: the peer stays routable.
        #[tokio::test]
        async fn handshake_failure_on_duplicate_leaves_healthy_active_entry() {
            let mut behaviour = test_behaviour();
            let overlay = test_overlay(1);
            let peer_id = activate_connection(&behaviour, overlay);
            assert_eq!(behaviour.connection_registry.active_count(), 1);

            // A second, unregistered connection to the same peer fails.
            let c2 = ConnectionId::new_unchecked(2);
            behaviour.process_protocol_event(peer_id, c2, failed_event(peer_id, c2));

            assert!(
                behaviour
                    .connection_registry
                    .get(&overlay)
                    .expect("active entry survives")
                    .is_active()
            );
            assert_eq!(
                ActivePeers::active_peer_id(&*behaviour.connection_registry, &overlay),
                Some(peer_id)
            );
            assert_eq!(behaviour.connection_registry.active_count(), 1);
        }

        /// An inbound duplicate to an already-active peer is refused, so a later
        /// handshake failure on it cannot clobber the identity index: the peer
        /// stays fully routable in both directions.
        #[tokio::test]
        async fn handshake_failure_on_refused_duplicate_leaves_peer_fully_routable() {
            let mut behaviour = test_behaviour();
            let overlay = test_overlay(1);
            let peer_id = activate_connection(&behaviour, overlay);

            // A duplicate inbound connection to the tracked peer registers nothing.
            let c2 = ConnectionId::new_unchecked(2);
            assert!(
                behaviour
                    .connection_registry
                    .connected_inbound(peer_id, c2)
                    .is_none()
            );
            assert_eq!(behaviour.connection_registry.pending_count(), 0);

            behaviour.process_protocol_event(peer_id, c2, failed_event(peer_id, c2));

            assert!(
                behaviour
                    .connection_registry
                    .get(&overlay)
                    .expect("active entry survives")
                    .is_active()
            );
            assert_eq!(
                ActivePeers::active_peer_id(&*behaviour.connection_registry, &overlay),
                Some(peer_id)
            );
            assert_eq!(
                ActivePeers::active_id(&*behaviour.connection_registry, &peer_id),
                Some(overlay)
            );
            assert_eq!(behaviour.connection_registry.active_count(), 1);
            assert_eq!(behaviour.connection_registry.pending_count(), 0);
        }

        /// An extra handshake exchange is a protocol violation: the peer is
        /// dropped through the close choke point with the recorded reason,
        /// and the registry teardown is left to the close handler.
        #[tokio::test]
        async fn unexpected_exchange_closes_the_connection_as_protocol_violation() {
            let mut behaviour = test_behaviour();
            let overlay = test_overlay(1);
            let peer_id = activate_connection(&behaviour, overlay);

            let c1 = ConnectionId::new_unchecked(1);
            behaviour.process_protocol_event(
                peer_id,
                c1,
                ProtocolEvent::Handshake(HandshakeEvent::Failed {
                    peer_id,
                    connection_id: c1,
                    direction: ConnectionDirection::Inbound,
                    error: HandshakeError::UnexpectedExchange,
                }),
            );

            assert_eq!(
                behaviour.pending_closes.get(&peer_id),
                Some(&DisconnectReason::ProtocolViolation)
            );
            match next_action(&mut behaviour).await {
                ToSwarm::CloseConnection {
                    peer_id: closed, ..
                } => assert_eq!(closed, peer_id),
                _ => panic!("expected CloseConnection for the violating peer"),
            }

            // The active entry survives until the close lands; the close
            // handler owns the teardown.
            assert!(
                behaviour
                    .connection_registry
                    .get(&overlay)
                    .expect("active entry survives until the close")
                    .is_active()
            );
        }

        /// A failed handshake on the peer's sole connection still removes its
        /// registry entry exactly as before.
        #[tokio::test]
        async fn handshake_failure_removes_the_sole_connection_entry() {
            let mut behaviour = test_behaviour();
            let peer_id = PeerId::random();
            let c1 = ConnectionId::new_unchecked(1);
            behaviour.connection_registry.connected_inbound(peer_id, c1);
            assert_eq!(behaviour.connection_registry.pending_count(), 1);

            behaviour.process_protocol_event(peer_id, c1, failed_event(peer_id, c1));

            assert!(!behaviour.connection_registry.contains_peer(&peer_id));
            assert_eq!(behaviour.connection_registry.pending_count(), 0);
        }
    }

    /// The connection phase gauges are set from the registry's authoritative
    /// counts after every mutation, so no hand-counted delta can drift them.
    mod phase_gauges {
        use super::*;

        use std::time::Instant;

        use libp2p::Multiaddr;
        use libp2p::swarm::ConnectionId;
        use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
        use vertex_net_peer_registry::ConnectionDirection;
        use vertex_swarm_net_handshake::{HandshakeEvent, HandshakeInfo};
        use vertex_swarm_test_utils::{test_overlay, test_peer_id, test_swarm_peer};

        use crate::DialReason;
        use crate::composed::ProtocolEvent;

        /// Read both phase gauges from a single snapshot. A snapshot resets
        /// gauges, so both must be read together; the sync helper writes
        /// absolute values, so a later read after a further sync is unaffected.
        fn phase_gauges(snapshotter: &Snapshotter) -> (f64, f64) {
            let mut pending = 0.0;
            let mut active = 0.0;
            for (ck, _, _, value) in snapshotter.snapshot().into_vec() {
                if let DebugValue::Gauge(g) = value {
                    match ck.key().name() {
                        "peer_registry_pending_connections" => pending = g.0,
                        "peer_registry_active_connections" => active = g.0,
                        _ => {}
                    }
                }
            }
            (pending, active)
        }

        /// A completed-handshake protocol event carrying `swarm_peer`. The outer
        /// peer_id/connection_id passed to `process_protocol_event` drive the
        /// handler; the event's own copies are unused.
        fn completed_event(swarm_peer: vertex_swarm_peer::SwarmPeer) -> ProtocolEvent {
            ProtocolEvent::Handshake(HandshakeEvent::Completed {
                peer_id: PeerId::random(),
                connection_id: ConnectionId::new_unchecked(0),
                direction: ConnectionDirection::Inbound,
                info: Box::new(HandshakeInfo {
                    peer_id: PeerId::random(),
                    swarm_peer,
                    node_type: SwarmNodeType::Client,
                    welcome_message: String::new(),
                    observed_multiaddr: Multiaddr::empty(),
                }),
            })
        }

        /// Read the gauges once (the read resets them) and assert they equal
        /// both the registry's authoritative counts and the expected truth.
        fn check(
            behaviour: &TopologyBehaviour<Identity>,
            snapshotter: &Snapshotter,
            expected: (f64, f64),
        ) {
            let got = phase_gauges(snapshotter);
            assert_eq!(
                got.0,
                behaviour.connection_registry.pending_count() as f64,
                "pending gauge must equal the registry pending count"
            );
            assert_eq!(
                got.1,
                behaviour.connection_registry.active_count() as f64,
                "active gauge must equal the registry active count"
            );
            assert_eq!(got, expected, "gauges must equal the expected truth");
        }

        /// Directly setting the gauges mirrors the registry counts exactly,
        /// whatever those counts are.
        #[test]
        fn sync_sets_both_gauges_to_registry_counts() {
            let behaviour = test_behaviour();
            let overlay = test_overlay(1);
            let peer_id = test_peer_id(1);
            behaviour
                .connection_registry
                .connected_inbound(peer_id, ConnectionId::new_unchecked(1));
            behaviour.connection_registry.activate(
                peer_id,
                ConnectionId::new_unchecked(1),
                overlay,
            );
            behaviour
                .connection_registry
                .connected_inbound(test_peer_id(2), ConnectionId::new_unchecked(2));
            assert_eq!(behaviour.connection_registry.active_count(), 1);
            assert_eq!(behaviour.connection_registry.pending_count(), 1);

            let recorder = DebuggingRecorder::new();
            let snapshotter = recorder.snapshotter();
            metrics::with_local_recorder(&recorder, || {
                behaviour.sync_connection_phase_gauges();
            });

            check(&behaviour, &snapshotter, (1.0, 1.0));
        }

        /// Drift (a): activating a peer that had both an overlay-keyed pending
        /// placeholder and its own pending entry consumes two pending entries.
        /// The old +1/-1 delta decremented pending only once and leaked +1; the
        /// sync sets pending to the true count of zero.
        #[test]
        fn two_pending_activation_does_not_leak_pending() {
            let mut behaviour = test_behaviour();
            let peer = test_swarm_peer(1);
            let overlay = OverlayAddress::from(*peer.overlay());
            let completing_peer = test_peer_id(1);
            let placeholder_peer = test_peer_id(9);
            let c_out = ConnectionId::new_unchecked(1);
            let c_in = ConnectionId::new_unchecked(2);

            behaviour.connection_registry.connected_outbound(
                placeholder_peer,
                c_out,
                Some(overlay),
                Instant::now(),
                Some(DialReason::Discovery),
            );
            behaviour
                .connection_registry
                .connected_inbound(completing_peer, c_in);
            assert_eq!(behaviour.connection_registry.pending_count(), 2);

            let recorder = DebuggingRecorder::new();
            let snapshotter = recorder.snapshotter();
            metrics::with_local_recorder(&recorder, || {
                behaviour.process_protocol_event(completing_peer, c_in, completed_event(peer));
            });

            assert_eq!(behaviour.connection_registry.pending_count(), 0);
            check(&behaviour, &snapshotter, (0.0, 1.0));
        }

        /// Drift (b): a refused duplicate registers nothing, so activating the
        /// existing peer over the duplicate connection consumes no pending
        /// entry. The old delta decremented pending anyway, driving it to -1;
        /// the sync holds it at the true zero.
        #[test]
        fn refused_duplicate_then_success_does_not_underflow_pending() {
            let mut behaviour = test_behaviour();
            let peer = test_swarm_peer(1);
            let overlay = OverlayAddress::from(*peer.overlay());
            let peer_id = test_peer_id(1);
            let c1 = ConnectionId::new_unchecked(1);
            let c2 = ConnectionId::new_unchecked(2);

            behaviour.connection_registry.connected_inbound(peer_id, c1);
            behaviour.connection_registry.activate(peer_id, c1, overlay);
            assert!(
                behaviour
                    .connection_registry
                    .connected_inbound(peer_id, c2)
                    .is_none()
            );
            assert_eq!(behaviour.connection_registry.pending_count(), 0);
            assert_eq!(behaviour.connection_registry.active_count(), 1);

            let recorder = DebuggingRecorder::new();
            let snapshotter = recorder.snapshotter();
            metrics::with_local_recorder(&recorder, || {
                behaviour.process_protocol_event(peer_id, c2, completed_event(peer));
            });

            assert_eq!(behaviour.connection_registry.pending_count(), 0);
            check(&behaviour, &snapshotter, (0.0, 1.0));
        }

        /// Drift (c): a racing dialer replaces an active peer under the same
        /// overlay. The registry nets active unchanged (one removed, one added),
        /// but the old code only decremented active for the superseded entry
        /// and never incremented for the new one, drifting active to -1. The
        /// sync keeps active at the true count.
        #[test]
        fn replacement_keeps_active_gauge_at_truth() {
            let mut behaviour = test_behaviour();
            let peer = test_swarm_peer(1);
            let overlay = OverlayAddress::from(*peer.overlay());
            let completing_peer = test_peer_id(1);
            let incumbent_peer = test_peer_id(9);
            let c1 = ConnectionId::new_unchecked(1);
            let c2 = ConnectionId::new_unchecked(2);

            // An incumbent peer holds the overlay's active slot.
            behaviour
                .connection_registry
                .connected_inbound(incumbent_peer, c1);
            behaviour
                .connection_registry
                .activate(incumbent_peer, c1, overlay);
            // A racing dialer connects inbound and completes the handshake for
            // the same overlay.
            behaviour
                .connection_registry
                .connected_inbound(completing_peer, c2);
            assert_eq!(behaviour.connection_registry.active_count(), 1);

            let recorder = DebuggingRecorder::new();
            let snapshotter = recorder.snapshotter();
            metrics::with_local_recorder(&recorder, || {
                behaviour.process_protocol_event(completing_peer, c2, completed_event(peer));
            });

            assert_eq!(behaviour.connection_registry.active_count(), 1);
            check(&behaviour, &snapshotter, (0.0, 1.0));
        }

        /// A mixed sequence of registrations, activations, and closes keeps both
        /// gauges pinned to the registry counts after every step.
        #[test]
        fn mixed_sequence_tracks_registry_counts() {
            let mut behaviour = test_behaviour();
            let recorder = DebuggingRecorder::new();
            let snapshotter = recorder.snapshotter();

            let peer_a = test_swarm_peer(1);
            let id_a = test_peer_id(1);
            let c_a = ConnectionId::new_unchecked(1);

            // Step 1: an inbound pending registration.
            behaviour.connection_registry.connected_inbound(id_a, c_a);
            metrics::with_local_recorder(&recorder, || {
                behaviour.sync_connection_phase_gauges();
            });
            check(&behaviour, &snapshotter, (1.0, 0.0));

            // Step 2: activation over that pending entry.
            metrics::with_local_recorder(&recorder, || {
                behaviour.process_protocol_event(id_a, c_a, completed_event(peer_a));
            });
            check(&behaviour, &snapshotter, (0.0, 1.0));

            // Step 3: a second inbound pending registration.
            let id_b = test_peer_id(2);
            let c_b = ConnectionId::new_unchecked(2);
            behaviour.connection_registry.connected_inbound(id_b, c_b);
            metrics::with_local_recorder(&recorder, || {
                behaviour.sync_connection_phase_gauges();
            });
            check(&behaviour, &snapshotter, (1.0, 1.0));

            // Step 4: the stale pending entry is garbage-collected.
            metrics::with_local_recorder(&recorder, || {
                behaviour
                    .connection_registry
                    .disconnected(&id_b)
                    .expect("pending entry present");
                behaviour.sync_connection_phase_gauges();
            });
            check(&behaviour, &snapshotter, (0.0, 1.0));

            // Step 5: the active peer disconnects through the close handler.
            let endpoint = libp2p::core::ConnectedPoint::Listener {
                local_addr: "/ip4/127.0.0.1/tcp/1".parse().expect("valid"),
                send_back_addr: "/ip4/127.0.0.2/tcp/2".parse().expect("valid"),
            };
            metrics::with_local_recorder(&recorder, || {
                behaviour.handle_connection_closed(libp2p::swarm::behaviour::ConnectionClosed {
                    peer_id: id_a,
                    connection_id: c_a,
                    endpoint: &endpoint,
                    cause: None,
                    remaining_established: 0,
                });
            });
            check(&behaviour, &snapshotter, (0.0, 0.0));
        }
    }

    mod early_disconnect {
        use std::io;

        use libp2p::core::ConnectedPoint;
        use libp2p::swarm::ConnectionId;
        use libp2p::swarm::behaviour::ConnectionClosed;
        use vertex_net_peer_registry::ConnectionDirection;
        use vertex_swarm_api::{ReportSource, SwarmScoringEvent};
        use vertex_swarm_test_utils::{test_overlay, test_swarm_peer};

        use super::*;

        fn conn() -> ConnectionId {
            ConnectionId::new_unchecked(1)
        }

        /// Register an active, peer-manager-known connection for overlay `n`.
        fn connect(behaviour: &TopologyBehaviour<Identity>, n: u8) -> (OverlayAddress, PeerId) {
            let overlay = test_overlay(n);
            let peer_id = PeerId::random();
            behaviour
                .connection_registry
                .connected_inbound(peer_id, conn());
            behaviour
                .connection_registry
                .activate(peer_id, conn(), overlay);
            behaviour.peer_manager.on_peer_connected(
                test_swarm_peer(n),
                SwarmNodeType::Client,
                ConnectionDirection::Inbound,
                TrustLevel::Normal,
            );
            (overlay, peer_id)
        }

        fn close(
            behaviour: &mut TopologyBehaviour<Identity>,
            peer_id: PeerId,
            cause: Option<&libp2p::swarm::ConnectionError>,
        ) {
            let endpoint = ConnectedPoint::Listener {
                local_addr: "/ip4/127.0.0.1/tcp/1".parse().expect("valid"),
                send_back_addr: "/ip4/127.0.0.2/tcp/2".parse().expect("valid"),
            };
            behaviour.handle_connection_closed(ConnectionClosed {
                peer_id,
                connection_id: conn(),
                endpoint: &endpoint,
                cause,
                remaining_established: 0,
            });
        }

        fn reset() -> libp2p::swarm::ConnectionError {
            libp2p::swarm::ConnectionError::IO(io::Error::from(io::ErrorKind::ConnectionReset))
        }

        /// A fast remote close of a peer that did nothing is the one case the
        /// early-disconnect penalty exists for.
        #[test]
        fn remote_close_of_idle_peer_is_penalized() {
            let mut behaviour = test_behaviour();
            let (overlay, peer_id) = connect(&behaviour, 1);
            let before = behaviour.peer_manager.get_peer_score(&overlay).unwrap();
            close(&mut behaviour, peer_id, Some(&reset()));
            let after = behaviour.peer_manager.get_peer_score(&overlay).unwrap();
            assert!(after < before, "idle remote close must be penalized");
        }

        /// A keep-alive teardown is our own idle drop, not a peer fault.
        #[test]
        fn idle_timeout_is_not_penalized() {
            let mut behaviour = test_behaviour();
            let (overlay, peer_id) = connect(&behaviour, 1);
            let before = behaviour.peer_manager.get_peer_score(&overlay).unwrap();
            let cause = libp2p::swarm::ConnectionError::KeepAliveTimeout;
            close(&mut behaviour, peer_id, Some(&cause));
            let after = behaviour.peer_manager.get_peer_score(&overlay).unwrap();
            assert_eq!(after, before, "idle teardown must not penalize the peer");
        }

        /// A close we initiated (recorded as intent) is blameless even inside
        /// the early-disconnect window, and the intent is consumed.
        #[test]
        fn local_intent_close_is_not_penalized() {
            let mut behaviour = test_behaviour();
            let (overlay, peer_id) = connect(&behaviour, 1);
            let before = behaviour.peer_manager.get_peer_score(&overlay).unwrap();
            behaviour
                .pending_closes
                .insert(peer_id, DisconnectReason::BinTrimmed);
            close(&mut behaviour, peer_id, None);
            let after = behaviour.peer_manager.get_peer_score(&overlay).unwrap();
            assert_eq!(after, before, "our own close must not penalize the peer");
            assert!(
                behaviour.pending_closes.is_empty(),
                "intent must be consumed at the close"
            );
        }

        /// A peer that served a chunk is blameless however the connection ends.
        #[test]
        fn productive_peer_is_not_penalized_on_remote_close() {
            let mut behaviour = test_behaviour();
            let (overlay, peer_id) = connect(&behaviour, 1);
            behaviour.peer_manager.report_peer(
                &overlay,
                SwarmScoringEvent::RetrievalSuccess {
                    latency: Duration::from_millis(5),
                },
                ReportSource::Protocol("retrieval"),
            );
            let before = behaviour.peer_manager.get_peer_score(&overlay).unwrap();
            close(&mut behaviour, peer_id, Some(&reset()));
            let after = behaviour.peer_manager.get_peer_score(&overlay).unwrap();
            assert_eq!(after, before, "a serving peer must not be penalized");
        }
    }

    mod dial_rate {
        use super::*;

        use std::num::NonZeroU32;

        use vertex_net_ratelimiter::Quota;
        use vertex_swarm_test_utils::test_swarm_peer;

        fn nz(n: u32) -> NonZeroU32 {
            NonZeroU32::new(n).expect("non-zero")
        }

        /// Build a behaviour whose dial-rate bucket holds `burst` tokens over
        /// `window`, with a known loopback listen address so candidate
        /// multiaddrs pass the reachability filter.
        fn throttled_behaviour(burst: u32, window: Duration) -> TopologyBehaviour<Identity> {
            let behaviour = test_behaviour_with(
                TopologyConfig::default().with_dial_quota(Quota::n_every(nz(burst), window)),
            );
            behaviour
                .nat_discovery
                .on_new_listen_addr("/ip4/127.0.0.1/tcp/1634".parse().expect("valid multiaddr"));
            behaviour
        }

        /// Store a dialable loopback peer and queue it as a dial candidate.
        fn queue_candidate(behaviour: &TopologyBehaviour<Identity>, n: u8) {
            let peer = test_swarm_peer(n);
            let overlay = OverlayAddress::from(*peer.overlay());
            behaviour.peer_manager.store_discovered_peer(peer);
            behaviour.routing.requeue_candidate(overlay);
        }

        /// Poll the behaviour once with a no-op waker.
        fn poll_once(
            behaviour: &mut TopologyBehaviour<Identity>,
        ) -> Poll<ToSwarm<(), THandlerInEvent<ProtocolBehaviours<Identity>>>> {
            let waker = futures::task::noop_waker();
            let mut cx = Context::from_waker(&waker);
            behaviour.poll(&mut cx)
        }

        /// The bucket absorbs a burst up to its size, then defers the rest:
        /// the third candidate stays queued and a wake-up timer is armed.
        #[tokio::test]
        async fn burst_dials_immediately_then_throttles() {
            let mut behaviour = throttled_behaviour(2, Duration::from_secs(600));
            for n in [0x10, 0x20, 0x30] {
                queue_candidate(&behaviour, n);
            }

            let mut dials = 0;
            loop {
                match poll_once(&mut behaviour) {
                    Poll::Ready(ToSwarm::Dial { .. }) => dials += 1,
                    Poll::Ready(_) => {}
                    Poll::Pending => break,
                }
            }

            assert_eq!(dials, 2, "burst must equal the bucket size");
            assert!(
                behaviour.dial_rate_timer.is_some(),
                "throttled drain must arm the replenish timer"
            );
            assert!(
                behaviour.routing.pop_candidate().is_some(),
                "the throttled candidate must stay queued"
            );
        }

        /// When tokens replenish, the armed timer wakes the poll loop and the
        /// deferred candidate is dialed without waiting for the next
        /// evaluation tick.
        #[tokio::test]
        async fn timer_resumes_drain_after_replenish() {
            // 1 token per 100ms of real time: the second candidate defers,
            // then drains when the timer fires.
            let mut behaviour = throttled_behaviour(1, Duration::from_millis(100));
            queue_candidate(&behaviour, 0x10);
            queue_candidate(&behaviour, 0x20);

            match next_action(&mut behaviour).await {
                ToSwarm::Dial { .. } => {}
                other => panic!("expected first Dial, got {other:?}"),
            }
            assert!(behaviour.dial_rate_timer.is_some());

            // The awaited poll wakes via the armed timer once a token is back.
            match next_action(&mut behaviour).await {
                ToSwarm::Dial { .. } => {}
                other => panic!("expected deferred Dial after replenish, got {other:?}"),
            }
        }
    }

    mod dial_surface {
        use super::*;

        use vertex_swarm_test_utils::{MockIdentity, test_overlay, test_swarm_peer};

        use crate::dial_state::DialState;
        use crate::handle::TopologyHandle;
        use crate::kademlia::{RoutingCapacity, SwarmRouting};

        /// Build a behaviour and its handle over a fixed-overlay mock identity
        /// so proximity bins are controllable.
        fn mock_behaviour(
            node_type: SwarmNodeType,
            config: TopologyConfig,
        ) -> (
            TopologyBehaviour<MockIdentity>,
            TopologyHandle<MockIdentity>,
        ) {
            let identity = MockIdentity::with_overlay(test_overlay(0)).with_node_type(node_type);
            TopologyBehaviourBuilder::new(identity, &EventTestConfig::new())
                .with_config(config)
                .try_build()
                .expect("build without runtime")
        }

        fn poll_once_mock(
            behaviour: &mut TopologyBehaviour<MockIdentity>,
        ) -> Poll<ToSwarm<(), THandlerInEvent<ProtocolBehaviours<MockIdentity>>>> {
            let waker = futures::task::noop_waker();
            let mut cx = Context::from_waker(&waker);
            behaviour.poll(&mut cx)
        }

        /// Send a dial-state query and poll the behaviour once so the reply is
        /// produced inside the poll, then return the snapshot.
        async fn query_dial_state(
            behaviour: &mut TopologyBehaviour<MockIdentity>,
            handle: &TopologyHandle<MockIdentity>,
        ) -> DialState {
            let mut fut = Box::pin(handle.dial_state());
            // First poll enqueues the command (buffered) and parks on the reply.
            assert!(futures::poll!(fut.as_mut()).is_pending());
            // The behaviour processes the command before it drains candidates,
            // so the snapshot is taken against pre-drain state.
            let _ = poll_once_mock(behaviour);
            match futures::poll!(fut.as_mut()) {
                Poll::Ready(Ok(state)) => state,
                other => panic!("dial_state must resolve after one poll, got {other:?}"),
            }
        }

        /// A forced evaluation from the handle wakes a parked behaviour and
        /// runs the tick body (here the isolation redial) without waiting for
        /// the hour-long dial tick, proving the command reuses the tick path.
        #[tokio::test]
        async fn trigger_evaluation_wakes_a_parked_behaviour_without_a_tick() {
            let identity =
                Identity::random(vertex_swarm_spec::init_testnet(), SwarmNodeType::Client);
            let (mut behaviour, handle) =
                TopologyBehaviourBuilder::new(identity, &EventTestConfig::new())
                    .with_config(
                        TopologyConfig::default().with_dial_interval(Duration::from_secs(3600)),
                    )
                    .try_build()
                    .expect("build without runtime");

            // A literal bootnode with a `/p2p/` component so the redial can
            // surface its peer id. The table is drained, so a forced round's
            // isolation probe redials it.
            let bootnode_peer = PeerId::random();
            let bootnode: Multiaddr = format!("/ip4/203.0.113.9/tcp/1634/p2p/{bootnode_peer}")
                .parse()
                .expect("valid bootnode multiaddr");
            behaviour.bootnodes = vec![bootnode];

            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                handle
                    .trigger_evaluation()
                    .await
                    .expect("trigger evaluation");
            });

            let action = tokio::time::timeout(
                Duration::from_secs(2),
                std::future::poll_fn(|cx| behaviour.poll(cx)),
            )
            .await
            .expect("the forced round must wake the parked behaviour");
            match action {
                ToSwarm::Dial { opts } => assert_eq!(opts.get_peer_id(), Some(bootnode_peer)),
                other => panic!("expected the forced round to redial the bootnode, got {other:?}"),
            }
        }

        /// `run_evaluation_round` runs the whole tick body: a deferred depth
        /// lowering whose window has expired publishes through it.
        #[tokio::test(start_paused = true)]
        async fn run_evaluation_round_publishes_a_deferred_depth_lowering() {
            let (mut behaviour, _handle) = mock_behaviour(
                SwarmNodeType::Storer,
                TopologyConfig::default().with_dial_interval(Duration::from_secs(3600)),
            );

            // Saturate bin 0 (8 peers at proximity order 0) with bins 1..=3
            // holding the neighborhood so the depth anchors at 1.
            for n in 0x80..0x88 {
                SwarmRouting::connected(&*behaviour.routing, test_overlay(n));
            }
            for n in 0x40..0x45 {
                SwarmRouting::connected(&*behaviour.routing, test_overlay(n));
            }
            for n in 0x20..0x23 {
                SwarmRouting::connected(&*behaviour.routing, test_overlay(n));
            }
            SwarmRouting::connected(&*behaviour.routing, test_overlay(0x10));
            assert_eq!(behaviour.routing.depth().get(), 1);

            // Drop one bin-0 peer: a single-peer deficit defers the lowering,
            // holding the published depth at 1 with a pending window.
            SwarmRouting::on_peer_disconnected(&*behaviour.routing, &test_overlay(0x80));
            assert_eq!(behaviour.routing.depth().get(), 1);

            let mut events = behaviour.event_tx.subscribe();

            // Past the stability window, the forced round publishes the lower
            // depth (default window is well under this advance).
            tokio::time::advance(Duration::from_secs(600)).await;
            behaviour.run_evaluation_round();

            let event = events.try_recv().expect("a depth change must be published");
            match event {
                TopologyEvent::DepthChanged {
                    old_depth,
                    new_depth,
                } => {
                    assert_eq!(old_depth, 1);
                    assert_eq!(new_depth, 0);
                }
                other => panic!("expected DepthChanged, got {other:?}"),
            }
            assert_eq!(behaviour.routing.depth().get(), 0);
        }

        /// The snapshot reads seeded connection, backoff, queue, slot, and
        /// bucket state from the authoritative tables, and its token counts
        /// track the drain.
        #[tokio::test]
        async fn dial_state_reflects_seeded_state() {
            use std::num::NonZeroU32;

            use vertex_net_ratelimiter::Quota;

            // Burst of one so the drain empties the bucket in a single dial.
            let (mut behaviour, handle) = mock_behaviour(
                SwarmNodeType::Storer,
                TopologyConfig::default()
                    .with_dial_interval(Duration::from_secs(3600))
                    .with_dial_quota(Quota::n_every(
                        NonZeroU32::new(1).expect("non-zero"),
                        Duration::from_secs(600),
                    )),
            );
            behaviour
                .nat_discovery
                .on_new_listen_addr("/ip4/127.0.0.1/tcp/1634".parse().expect("valid multiaddr"));

            // Bin 0: one self-dialed (outbound) peer and one connected-only
            // peer, so connected is two but only one is outbound.
            let outbound = test_overlay(0x80);
            SwarmRouting::connected(&*behaviour.routing, outbound);
            assert!(
                behaviour
                    .routing
                    .try_reserve_dial(&outbound, SwarmNodeType::Storer)
            );
            behaviour.routing.dial_connected(&outbound);
            behaviour.routing.handshake_completed(&outbound);
            SwarmRouting::connected(&*behaviour.routing, test_overlay(0x81));

            // One known peer serving a dial backoff (bin 1).
            let backed_off = behaviour
                .peer_manager
                .store_discovered_peer(test_swarm_peer(0x40));
            behaviour.peer_manager.record_dial_failure(&backed_off);
            assert!(behaviour.peer_manager.peer_is_in_backoff(&backed_off));

            // Two dialable candidates queued in bin 0.
            for n in [0xC0u8, 0xC1] {
                let peer = test_swarm_peer(n);
                let overlay = OverlayAddress::from(*peer.overlay());
                behaviour.peer_manager.store_discovered_peer(peer);
                behaviour.routing.requeue_candidate(overlay);
            }

            let bin0 = Bin::new(0).expect("valid bin");
            let expected_slots = behaviour.routing.filled_slots(bin0);
            let expected_min_outbound = behaviour.routing.limits().min_outbound();

            // First snapshot: taken before the poll's drain touches the bucket.
            let fresh = query_dial_state(&mut behaviour, &handle).await;
            assert_eq!(fresh.queued_candidates, 2);
            assert_eq!(fresh.peers_in_backoff, 1);
            assert_eq!(fresh.eligible_known_peers, 3);
            assert_eq!(fresh.available_dial_tokens, 1);
            assert_eq!(fresh.next_dial_token_in, None);
            assert!(!fresh.throttled);
            assert_eq!(fresh.last_evaluation, None);
            assert_eq!(fresh.min_outbound, expected_min_outbound);

            let fresh_bin0 = fresh
                .bins
                .iter()
                .find(|b| b.bin == bin0)
                .expect("bin 0 present");
            assert_eq!(fresh_bin0.connected, 2);
            assert_eq!(fresh_bin0.outbound, 1);
            assert_eq!(fresh_bin0.queued, 2);
            // Neighborhood bins report no slot coverage; bin 0 sits below the
            // depth-0 frontier so it carries a finite target and slot count
            // that matches the readiness snapshot.
            let readiness = handle.readiness();
            let readiness_bin0 = readiness
                .bins
                .iter()
                .find(|b| b.bin == bin0)
                .expect("bin 0 present in readiness");
            assert_eq!(fresh_bin0.slots_filled, readiness_bin0.slots_filled);
            assert_eq!(fresh_bin0.slots_filled, Some(expected_slots));

            // The first poll consumed the single token dialing one candidate;
            // the second snapshot sees the drained bucket and shorter queue.
            let drained = query_dial_state(&mut behaviour, &handle).await;
            assert_eq!(drained.available_dial_tokens, 0);
            assert!(drained.next_dial_token_in.is_some());
            assert!(drained.throttled);
            assert_eq!(drained.queued_candidates, 1);
        }
    }

    mod gossip_engine {
        use super::*;

        use libp2p::swarm::ConnectionId;
        use vertex_swarm_test_utils::{test_overlay, test_swarm_peer};

        /// An engine action applied by a connection event reaches the hive
        /// broadcast queue synchronously, and the very next poll emits it:
        /// no channel sits between gossip policy and the wire.
        #[tokio::test]
        async fn gossip_actions_apply_in_the_same_poll_cycle() {
            let mut behaviour = test_behaviour_with(TopologyConfig::default());

            let peer_id = PeerId::random();
            let connection_id = ConnectionId::new_unchecked(1);
            let recipient = test_overlay(0xAB);
            behaviour
                .connection_registry
                .connected_inbound(peer_id, connection_id);
            behaviour
                .connection_registry
                .activate(peer_id, connection_id, recipient);

            behaviour.apply_gossip_actions(vec![crate::gossip::GossipAction {
                to: recipient,
                peers: vec![test_swarm_peer(9)],
            }]);

            match next_action(&mut behaviour).await {
                ToSwarm::NotifyHandler {
                    peer_id: notified, ..
                } => assert_eq!(
                    notified, peer_id,
                    "the hive broadcast targets the recipient"
                ),
                other => panic!("expected the gossip broadcast to emit, got {other:?}"),
            }
        }
    }

    mod isolation {
        use super::*;

        /// A drained table (nothing connected or pending, no candidates)
        /// re-dials the bootnodes on the evaluation tick without an external
        /// command: the all-backoff empty table must not be a fixed point.
        #[tokio::test]
        async fn isolation_redials_bootnodes_without_a_command() {
            let mut behaviour = test_behaviour_with(
                TopologyConfig::default().with_dial_interval(Duration::from_millis(25)),
            );
            let bootnode_peer = PeerId::random();
            let bootnode: Multiaddr = format!("/ip4/203.0.113.9/tcp/1634/p2p/{bootnode_peer}")
                .parse()
                .expect("valid bootnode multiaddr");
            behaviour.bootnodes = vec![bootnode];

            match next_action(&mut behaviour).await {
                ToSwarm::Dial { opts } => {
                    assert_eq!(opts.get_peer_id(), Some(bootnode_peer));
                }
                other => panic!("expected the isolation probe to dial the bootnode, got {other:?}"),
            }
        }
    }

    mod bootnode_redial {
        use super::*;

        use libp2p::core::transport::ListenerId;
        use libp2p::swarm::behaviour::NewListenAddr;

        /// Drain every dial action the behaviour currently has queued and
        /// return the peer ids dialed.
        fn drain_dials(behaviour: &mut TopologyBehaviour<Identity>) -> Vec<PeerId> {
            let waker = futures::task::noop_waker();
            let mut cx = Context::from_waker(&waker);
            let mut dialed = Vec::new();
            loop {
                match behaviour.poll(&mut cx) {
                    Poll::Ready(ToSwarm::Dial { opts }) => {
                        if let Some(peer_id) = opts.get_peer_id() {
                            dialed.push(peer_id);
                        }
                    }
                    Poll::Ready(_) => {}
                    Poll::Pending => break,
                }
            }
            dialed
        }

        /// A bootnode dial dropped while the dial capability is `ip: None`
        /// (no listen address yet) is re-issued once the first listen address
        /// makes the capability known, which is the only path that rescues a
        /// node whose only contacts are bootnodes on a real network.
        #[tokio::test]
        async fn bootnode_redialed_when_capability_becomes_known() {
            let mut behaviour = test_behaviour_listening();

            // A literal bootnode with a `/p2p/` component so `dial()` can
            // extract its peer id.
            let bootnode_peer = PeerId::random();
            let bootnode: Multiaddr = format!("/ip4/203.0.113.7/tcp/1634/p2p/{bootnode_peer}")
                .parse()
                .expect("valid bootnode multiaddr");
            behaviour.bootnodes = vec![bootnode];

            // Startup path: ConnectBootnodes is processed before any listen
            // address arrives, so the dial capability ip is still None and the
            // dial is filtered out as unreachable.
            behaviour.on_command(TopologyCommand::ConnectBootnodes);
            assert!(
                drain_dials(&mut behaviour).is_empty(),
                "bootnode dial must be dropped while capability ip is None"
            );

            // First listen address arrives: capability becomes known and the
            // dropped bootnode dial is re-issued.
            let listen_addr: Multiaddr = "/ip4/192.0.2.1/tcp/1634"
                .parse()
                .expect("valid listen multiaddr");
            behaviour.on_swarm_event(FromSwarm::NewListenAddr(NewListenAddr {
                listener_id: ListenerId::next(),
                addr: &listen_addr,
            }));

            let dialed = drain_dials(&mut behaviour);
            assert!(
                dialed.contains(&bootnode_peer),
                "bootnode must be dialed once the capability becomes known, dialed: {dialed:?}"
            );
        }

        /// The redial fires exactly once per capability transition: a second
        /// listen address on the same (already-known) capability does not
        /// re-dial a bootnode that is already being tracked, so no dial storm.
        #[tokio::test]
        async fn bootnode_not_redialed_on_subsequent_listen_addr() {
            let mut behaviour = test_behaviour_listening();

            let bootnode_peer = PeerId::random();
            let bootnode: Multiaddr = format!("/ip4/203.0.113.8/tcp/1634/p2p/{bootnode_peer}")
                .parse()
                .expect("valid bootnode multiaddr");
            behaviour.bootnodes = vec![bootnode];

            behaviour.on_command(TopologyCommand::ConnectBootnodes);
            assert!(drain_dials(&mut behaviour).is_empty());

            // First listen address: capability becomes known, bootnode dialed.
            let addr1: Multiaddr = "/ip4/192.0.2.1/tcp/1634".parse().expect("valid");
            behaviour.on_swarm_event(FromSwarm::NewListenAddr(NewListenAddr {
                listener_id: ListenerId::next(),
                addr: &addr1,
            }));
            assert!(drain_dials(&mut behaviour).contains(&bootnode_peer));

            // Second listen address: capability is already known, so the
            // redial path does not fire. The bootnode is now in the dial
            // tracker, so even if connect_bootnodes ran it would be skipped;
            // assert no new dial is emitted.
            let addr2: Multiaddr = "/ip4/192.0.2.2/tcp/1634".parse().expect("valid");
            behaviour.on_swarm_event(FromSwarm::NewListenAddr(NewListenAddr {
                listener_id: ListenerId::next(),
                addr: &addr2,
            }));
            assert!(
                drain_dials(&mut behaviour).is_empty(),
                "no redial storm: bootnode already tracked after first transition"
            );
        }
    }
}
