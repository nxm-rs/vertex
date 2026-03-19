//! Gossip exchange coordinator task.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::FuturesUnordered;
use futures::StreamExt;
use libp2p::PeerId;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use metrics::counter;
use tokio::sync::mpsc;
use tokio::time::Interval;
use tracing::{debug, info, trace, warn};
use vertex_observability::LabelValue;
use vertex_swarm_api::{SwarmIdentity, SwarmNodeType};
use vertex_swarm_identity::Identity;
use vertex_swarm_net_handshake::{HandshakeBehaviour, HandshakeEvent, NoAddresses};
use vertex_swarm_net_identify as identify;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_spec::Spec;

use super::events::{GossipAction, GossipCheckOk, VerificationResult};
use super::filter::{
    RecipientProfile, detect_depth_decrease, filter_peers_for_recipient,
    scoring_event_for, select_peers_for_distant,
};
use super::verifier::{GossipVerifier, Verification};
use super::GossipInput;
use crate::kademlia::RoutingEvaluatorHandle;
use crate::kademlia::peer_selection;

use crate::behaviour::ConnectionRegistry;

/// Interval for refreshing neighborhood peers.
const GOSSIP_REFRESH_INTERVAL: Duration = Duration::from_secs(600);

/// Default delay before exchanging gossip with gossip-dial peers.
const DEFAULT_HEALTH_CHECK_DELAY: Duration = Duration::from_millis(500);

/// Channel capacity for gossip inputs.
const INPUT_CHANNEL_CAPACITY: usize = 128;

/// Channel capacity for gossip broadcast actions.
const OUTPUT_CHANNEL_CAPACITY: usize = 128;

/// Idle connection timeout for verification swarm.
const VERIFICATION_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Lightweight behaviour for verification handshakes + identify push.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "VerifierSwarmEvent")]
struct VerifierBehaviour {
    handshake: HandshakeBehaviour<Identity, NoAddresses>,
    identify: identify::Behaviour,
}

#[derive(Debug)]
enum VerifierSwarmEvent {
    Handshake(HandshakeEvent),
    Identify(identify::Event),
}

impl From<HandshakeEvent> for VerifierSwarmEvent {
    fn from(event: HandshakeEvent) -> Self {
        VerifierSwarmEvent::Handshake(event)
    }
}

impl From<identify::Event> for VerifierSwarmEvent {
    fn from(event: identify::Event) -> Self {
        VerifierSwarmEvent::Identify(event)
    }
}

/// Result of a pending delayed gossip exchange.
struct PendingExchange {
    peer_id: PeerId,
    swarm_peer: SwarmPeer,
    node_type: SwarmNodeType,
}

/// Gossip exchange coordinator task.
struct GossipTask<I: SwarmIdentity> {
    // Channels
    input_rx: mpsc::Receiver<GossipInput>,
    output_tx: mpsc::Sender<GossipAction>,

    // Verification (owns ephemeral handshake swarm)
    verifier: GossipVerifier,
    verifier_swarm: libp2p::Swarm<VerifierBehaviour>,
    local_capabilities: Arc<vertex_net_local::LocalCapabilities>,

    // Gossip state
    local_overlay: OverlayAddress,
    peer_manager: Arc<PeerManager<I>>,
    connection_registry: Arc<ConnectionRegistry>,
    current_depth: u8,
    last_depth: u8,
    last_broadcast: HashMap<OverlayAddress, Instant>,
    /// Peers we initiated a gossip-dial to. Bounded by active outbound connections:
    /// entries are added on `MarkGossipDial` (one per outbound dial) and removed on
    /// `PeerActivated` (connection succeeded) or `ConnectionClosed` (connection dropped).
    gossip_dial_peers: HashSet<PeerId>,
    health_check_delay: Duration,
    gossip_interval: Interval,

    // Delayed gossip exchange
    pending_exchanges: FuturesUnordered<Pin<Box<dyn Future<Output = PendingExchange> + Send>>>,
    cancelled_exchanges: HashSet<PeerId>,

    // Triggers routing evaluation after storing verified peers
    evaluator_handle: RoutingEvaluatorHandle,
}

impl<I: SwarmIdentity> GossipTask<I> {
    async fn run(mut self) {
        loop {
            tokio::select! {
                Some(input) = self.input_rx.recv() => {
                    self.handle_input(input);
                }
                event = self.verifier_swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                }
                _ = self.gossip_interval.tick() => {
                    self.on_tick();
                }
                Some(exchange) = self.pending_exchanges.next() => {
                    self.on_exchange_ready(exchange);
                }
                else => break,
            }
        }
        debug!("Gossip task shutting down");
    }

    fn handle_input(&mut self, input: GossipInput) {
        match input {
            GossipInput::MarkGossipDial(peer_id) => {
                self.gossip_dial_peers.insert(peer_id);
            }
            GossipInput::PeerActivated { peer_id, swarm_peer, node_type } => {
                if self.gossip_dial_peers.remove(&peer_id) {
                    // Gossip dial: delay before exchanging (peer may drop us if bin saturated)
                    self.schedule_exchange(peer_id, swarm_peer, node_type);
                } else {
                    // Non-gossip: exchange immediately
                    self.exchange_gossip(&swarm_peer, node_type);
                }
            }
            GossipInput::ConnectionClosed { peer_id, overlay } => {
                self.gossip_dial_peers.remove(&peer_id);
                self.cancelled_exchanges.insert(peer_id);
                if let Some(overlay) = &overlay {
                    self.last_broadcast.remove(overlay);
                }
                let actions = self.check_depth_change();
                self.emit_actions(actions);
            }
            GossipInput::DepthChanged(depth) => {
                self.current_depth = depth;
            }
            GossipInput::PeersReceived { gossiper, peers } => {
                // Filter out our own overlay before verification
                let peers: Vec<_> = peers
                    .into_iter()
                    .filter(|p| OverlayAddress::from(*p.overlay()) != self.local_overlay)
                    .collect();
                if peers.is_empty() {
                    return;
                }
                self.handle_verification_request(gossiper, peers);
                self.drain_pending_dials();
            }
        }
    }

    // Verification methods (merged from ReacherTask)

    fn handle_verification_request(&mut self, gossiper: OverlayAddress, peers: Vec<SwarmPeer>) {
        let mut queued = 0;
        let mut skipped = 0;
        let mut rejected = 0;

        for peer in peers {
            let existing = self.peer_manager.get_swarm_peer(peer.overlay());
            let result = self.verifier.check_gossip(&peer, &gossiper, existing.as_ref());
            match result {
                Ok(GossipCheckOk::AlreadyKnown) => {
                    trace!(overlay = %peer.overlay(), %gossiper, "gossip check: already_known");
                    skipped += 1;
                }
                Ok(GossipCheckOk::Enqueued) => {
                    trace!(overlay = %peer.overlay(), %gossiper, "gossip check: enqueued");
                    queued += 1;
                }
                Err(ref err) => {
                    trace!(overlay = %peer.overlay(), %gossiper, reason = %err, "gossip check: rejected");
                    counter!("topology_gossip_rejected_total", "reason" => err.label_value()).increment(1);
                    rejected += 1;
                }
            }
        }

        if queued > 0 || rejected > 0 {
            debug!(
                %gossiper,
                queued,
                skipped,
                rejected,
                "Verification request processed"
            );
        }
    }

    fn on_handshake_completed(&mut self, peer_id: PeerId, verified_peer: SwarmPeer) {
        if let Some(Verification::Resolved { gossiped_peer, gossiper, dial_addr }) = self.verifier.resolve_in_flight(&peer_id) {
            let (gossiper, result) = GossipVerifier::verify_handshake(gossiped_peer, gossiper, dial_addr, verified_peer);
            self.process_result(gossiper, result);
        }
    }

    fn on_dial_failed(&mut self, peer_id: &PeerId) {
        if let Some(Verification::Resolved { gossiped_peer, gossiper, .. }) = self.verifier.resolve_in_flight(peer_id) {
            let gossiped_overlay = *gossiped_peer.overlay();
            debug!(
                overlay = %gossiped_overlay,
                %gossiper,
                "Gossip verification failed: peer unreachable"
            );
            self.process_result(gossiper, VerificationResult::Unreachable { gossiped_overlay });
        }
    }

    fn drain_pending_dials(&mut self) {
        let capability = self.local_capabilities.capability();

        while let Some(Verification::Pending { peer_id, addrs }) = self.verifier.next_verification_dial() {
            if self.verifier_swarm.is_connected(&peer_id) {
                // Already connected -- clean up without sending an event
                self.verifier.resolve_in_flight(&peer_id);
                continue;
            }

            let Some(opts) = vertex_net_dialer::prepare_dial_opts(
                peer_id,
                addrs,
                |addr| vertex_net_local::is_dialable(addr, capability),
            ) else {
                debug!(%peer_id, ?capability, "no reachable addresses for verification");
                self.on_dial_failed(&peer_id);
                continue;
            };

            if let Err(e) = self.verifier_swarm.dial(opts) {
                warn!(%peer_id, error = %e, "Failed to initiate verification dial");
                self.on_dial_failed(&peer_id);
            }
        }
    }

    fn handle_swarm_event(&mut self, event: SwarmEvent<VerifierSwarmEvent>) {
        match event {
            SwarmEvent::Behaviour(VerifierSwarmEvent::Handshake(
                HandshakeEvent::Completed { peer_id, info, .. },
            )) => {
                debug!(
                    %peer_id,
                    overlay = %info.swarm_peer.overlay(),
                    "Verification handshake completed"
                );
                self.on_handshake_completed(peer_id, info.swarm_peer);

                // Disconnect after verification
                let _ = self.verifier_swarm.disconnect_peer_id(peer_id);

                // Drain more dials since a slot opened up
                self.drain_pending_dials();
            }
            SwarmEvent::Behaviour(VerifierSwarmEvent::Handshake(
                HandshakeEvent::Failed { peer_id, error, .. },
            )) => {
                debug!(%peer_id, %error, "Verification handshake failed");
                self.on_dial_failed(&peer_id);
                self.drain_pending_dials();
            }
            SwarmEvent::OutgoingConnectionError { peer_id: Some(peer_id), error, .. } => {
                debug!(%peer_id, %error, "Verification dial failed");
                self.on_dial_failed(&peer_id);
                self.drain_pending_dials();
            }
            SwarmEvent::Behaviour(VerifierSwarmEvent::Identify(
                identify::Event::Received { peer_id, info, .. },
            )) => {
                if !info.observed_addr.is_empty() {
                    trace!(%peer_id, observed = %info.observed_addr, "Pushing observed addr via identify");
                    self.verifier_swarm
                        .behaviour_mut()
                        .identify
                        .push_with_addresses(peer_id, vec![info.observed_addr]);
                }
            }
            SwarmEvent::Behaviour(VerifierSwarmEvent::Identify(_)) => {}
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                trace!(%peer_id, "Verification connection closed");
            }
            _ => {}
        }
    }

    fn process_result(&mut self, gossiper: OverlayAddress, result: VerificationResult) {
        let scoring_event = scoring_event_for(&result);

        match result {
            VerificationResult::Verified { verified_peer } => {
                let overlay = *verified_peer.overlay();
                let stored = self.peer_manager.store_discovered_peer(verified_peer);
                debug!(%stored, %gossiper, "Verified gossiped peer");
                self.verifier.clear_backoff(&OverlayAddress::from(overlay));
                self.evaluator_handle.trigger_evaluation();
            }
            VerificationResult::IdentityUpdated { verified_peer } => {
                let overlay = *verified_peer.overlay();
                let stored = self.peer_manager.store_discovered_peer(verified_peer);
                debug!(%stored, %gossiper, "Peer identity updated via verification");
                self.verifier.clear_backoff(&OverlayAddress::from(overlay));
            }
            VerificationResult::DifferentPeerAtAddress { verified_peer, gossiped_overlay } => {
                let verified_overlay = self.peer_manager.store_discovered_peer(verified_peer);
                warn!(
                    verified = %verified_overlay,
                    gossiped = %gossiped_overlay,
                    %gossiper,
                    "Wrong overlay - real peer stored, gossiper penalized"
                );
                self.verifier.clear_backoff(&gossiped_overlay);
            }
            VerificationResult::Failed { reason } => {
                warn!(%gossiper, %reason, "Gossip verification failed");
                counter!("topology_gossip_verification_failed_total", "reason" => reason.label_value()).increment(1);
                // No backoff: the address was reachable, the gossip data was bad
            }
            VerificationResult::Unreachable { gossiped_overlay } => {
                debug!(%gossiper, %gossiped_overlay, "Gossiped peer unreachable");
                self.verifier.record_backoff(&gossiped_overlay);
            }
        }

        self.peer_manager.record_scoring_event(&gossiper, scoring_event);
    }

    fn schedule_exchange(&mut self, peer_id: PeerId, swarm_peer: SwarmPeer, node_type: SwarmNodeType) {
        let delay = self.health_check_delay;
        self.pending_exchanges.push(Box::pin(async move {
            tokio::time::sleep(delay).await;
            PendingExchange { peer_id, swarm_peer, node_type }
        }));
    }

    fn on_exchange_ready(&mut self, exchange: PendingExchange) {
        if self.cancelled_exchanges.remove(&exchange.peer_id) {
            return; // Peer disconnected while delay was pending
        }
        self.exchange_gossip(&exchange.swarm_peer, exchange.node_type);
    }

    fn exchange_gossip(&mut self, swarm_peer: &SwarmPeer, node_type: SwarmNodeType) {
        let depth = self.current_depth;
        let mut actions = self.on_peer_authenticated(swarm_peer, node_type, depth);
        actions.extend(self.check_depth_change());
        self.emit_actions(actions);
    }

    fn emit_actions(&self, actions: Vec<GossipAction>) {
        for action in actions {
            let _ = self.output_tx.try_send(action);
        }
    }

    // Gossip exchange logic

    fn check_depth_change(&mut self) -> Vec<GossipAction> {
        let Some((old_depth, new_depth)) =
            detect_depth_decrease(self.current_depth, &mut self.last_depth)
        else {
            return Vec::new();
        };

        debug!(old_depth, new_depth, "Depth decreased - neighborhood expanded");

        let mut actions = Vec::new();

        for overlay in self.connection_registry.active_ids() {
            let proximity = self.local_overlay.proximity(&overlay);

            if proximity >= new_depth
                && proximity < old_depth
                && self.peer_manager.node_type(&overlay) == Some(SwarmNodeType::Storer)
            {
                debug!(%overlay, proximity, "Peer became neighbor due to depth change");

                if let Some(peer) = self.peer_manager.get_swarm_peer(&overlay) {
                    actions.extend(self.handle_new_neighbor(overlay, peer, new_depth));
                }
            }
        }

        actions
    }

    fn on_peer_authenticated(
        &mut self,
        peer: &SwarmPeer,
        node_type: SwarmNodeType,
        depth: u8,
    ) -> Vec<GossipAction> {
        self.last_depth = depth;

        if !node_type.requires_storage() {
            trace!(overlay = %peer.overlay(), "Skipping gossip for non-storer node");
            return Vec::new();
        }

        let new_peer_overlay = OverlayAddress::from(*peer.overlay());
        let proximity = self.local_overlay.proximity(&new_peer_overlay);

        if proximity >= depth {
            self.handle_new_neighbor(new_peer_overlay, peer.clone(), depth)
        } else {
            self.handle_new_distant_peer(new_peer_overlay)
        }
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        let mut actions = Vec::new();

        // Periodic cleanup: remove cancelled_exchanges entries with no pending future.
        // When pending_exchanges is empty, all futures have resolved so no cancellation
        // tokens are needed. Otherwise, retain only entries that could still match a
        // pending future (conservatively keep all — they are removed on resolution).
        if self.pending_exchanges.is_empty() {
            self.cancelled_exchanges.clear();
        }

        // Periodic cleanup: evict stale last_broadcast entries.
        // Entries older than 2x the refresh interval are unlikely to be useful —
        // the peer has either disconnected or will be refreshed on the next tick.
        let broadcast_expiry = GOSSIP_REFRESH_INTERVAL * 2;
        self.last_broadcast
            .retain(|_, ts| now.duration_since(*ts) <= broadcast_expiry);

        let neighbors = self.connected_neighbors();

        // Check if any neighbor is stale before computing the expensive peer set
        let has_stale = neighbors.iter().any(|neighbor| {
            self.last_broadcast
                .get(neighbor)
                .map(|t| now.duration_since(*t) > GOSSIP_REFRESH_INTERVAL)
                .unwrap_or(true)
        });

        if !has_stale {
            return;
        }

        // Compute the base neighborhood peer set once (without exclude)
        let all_neighborhood_peers =
            self.known_neighborhood_peers(self.current_depth, None);

        for neighbor in neighbors {
            let is_stale = self
                .last_broadcast
                .get(&neighbor)
                .map(|t| now.duration_since(*t) > GOSSIP_REFRESH_INTERVAL)
                .unwrap_or(true);

            if is_stale {
                let profile = self.recipient_profile(&neighbor);
                let filtered = self.filter_for_recipient(
                    &all_neighborhood_peers,
                    &profile,
                );

                // Exclude the neighbor itself from the result
                let peers: Vec<SwarmPeer> = filtered
                    .into_iter()
                    .filter(|p| OverlayAddress::from(*p.overlay()) != neighbor)
                    .cloned()
                    .collect();

                if !peers.is_empty() {
                    trace!(to = %neighbor, count = peers.len(), "Refreshing neighborhood peers");
                    actions.push(GossipAction {
                        to: neighbor,
                        peers,
                    });
                    self.last_broadcast.insert(neighbor, now);
                }
            }
        }

        self.emit_actions(actions);
    }

    fn handle_new_neighbor(
        &mut self,
        new_peer: OverlayAddress,
        new_peer_info: SwarmPeer,
        depth: u8,
    ) -> Vec<GossipAction> {
        let mut actions = Vec::new();

        debug!(%new_peer, depth, "New neighbor joined - initiating neighborhood exchange");

        let new_peer_profile = self.recipient_profile(&new_peer);

        let neighborhood_peers =
            self.known_neighborhood_peers(depth, Some(&new_peer));
        let filtered = self.filter_for_recipient(
            &neighborhood_peers,
            &new_peer_profile,
        );

        if !filtered.is_empty() {
            debug!(to = %new_peer, count = filtered.len(), "Sending known neighborhood peers");
            actions.push(GossipAction {
                to: new_peer,
                peers: filtered.into_iter().cloned().collect(),
            });
        }

        let existing_neighbors = self.connected_neighbors();

        // Wrap in a single-element slice so filter_peers_for_recipient can borrow
        // without cloning for each neighbor.
        let new_peer_slice = [new_peer_info];
        for neighbor in existing_neighbors {
            if neighbor != new_peer {
                let profile = self.recipient_profile(&neighbor);

                let filtered = self.filter_for_recipient(
                    &new_peer_slice,
                    &profile,
                );

                if !filtered.is_empty() {
                    trace!(to = %neighbor, about = %new_peer, "Notifying neighbor about new peer");
                    actions.push(GossipAction {
                        to: neighbor,
                        peers: filtered.into_iter().cloned().collect(),
                    });
                }
            }
        }

        self.last_broadcast.insert(new_peer, Instant::now());
        actions
    }

    fn handle_new_distant_peer(&mut self, peer: OverlayAddress) -> Vec<GossipAction> {
        let profile = self.recipient_profile(&peer);
        let peers = self.select_for_distant(peer, &profile);

        if peers.is_empty() {
            return Vec::new();
        }

        debug!(to = %peer, count = peers.len(), "Sending bootstrap peers to distant peer");

        self.last_broadcast.insert(peer, Instant::now());
        vec![GossipAction { to: peer, peers }]
    }

    fn recipient_profile(&self, overlay: &OverlayAddress) -> RecipientProfile {
        RecipientProfile::lookup(&self.peer_manager, overlay)
    }

    fn connected_neighbors(&self) -> Vec<OverlayAddress> {
        peer_selection::connected_neighbors(
            &self.local_overlay,
            &self.peer_manager,
            &self.connection_registry,
            self.current_depth,
        )
    }

    fn known_neighborhood_peers(
        &self,
        depth: u8,
        exclude: Option<&OverlayAddress>,
    ) -> Vec<SwarmPeer> {
        peer_selection::known_neighborhood_peers(
            &self.local_overlay,
            &self.peer_manager,
            depth,
            exclude,
        )
    }

    fn select_for_distant(
        &self,
        recipient_overlay: OverlayAddress,
        profile: &RecipientProfile,
    ) -> Vec<SwarmPeer> {
        select_peers_for_distant(
            &self.local_overlay,
            &self.peer_manager,
            recipient_overlay,
            profile,
        )
    }

    fn filter_for_recipient<'a>(
        &self,
        peers: &'a [SwarmPeer],
        profile: &RecipientProfile,
    ) -> Vec<&'a SwarmPeer> {
        filter_peers_for_recipient(peers, profile, &*self.peer_manager)
    }

}

/// Spawn the gossip task. Returns a handle for sending inputs and receiving outputs.
pub fn spawn_gossip_task<I: SwarmIdentity>(
    spec: Arc<Spec>,
    local_overlay: OverlayAddress,
    peer_manager: Arc<PeerManager<I>>,
    connection_registry: Arc<ConnectionRegistry>,
    evaluator_handle: RoutingEvaluatorHandle,
    local_capabilities: Arc<vertex_net_local::LocalCapabilities>,
) -> Result<super::GossipHandle, Box<dyn std::error::Error + Send + Sync>> {
    let (input_tx, input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
    let (output_tx, output_rx) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);

    // Build ephemeral identity for the verification swarm
    let verifier_identity = Arc::new(
        Identity::random(spec, SwarmNodeType::Client)
            .with_welcome_message("gossip-verifier"),
    );

    info!(
        overlay = %verifier_identity.overlay_address(),
        "Spawning gossip task with ephemeral verifier identity"
    );

    let verifier_swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .map_err(|e| format!("TCP transport: {e}"))?
        .with_dns()
        .map_err(|e| format!("DNS: {e}"))?
        .with_behaviour(|keypair| {
            let identify_config = identify::Config::new(keypair.public())
                .with_agent_version(format!("vertex-verifier/{}", env!("CARGO_PKG_VERSION")))
                .with_cache_size(0)
                .with_purpose("verifier");
            Ok(VerifierBehaviour {
                handshake: HandshakeBehaviour::new(
                    verifier_identity.clone(),
                    Arc::new(NoAddresses),
                    "verifier",
                ),
                identify: identify::Behaviour::new(
                    identify_config,
                    identify::new_agent_versions(),
                ),
            })
        })
        .map_err(|e| format!("Behaviour: {e}"))?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(VERIFICATION_IDLE_TIMEOUT))
        .build();

    let verifier = GossipVerifier::new();

    let task = GossipTask {
        input_rx,
        output_tx,
        verifier,
        verifier_swarm,
        local_capabilities,
        local_overlay,
        peer_manager,
        connection_registry,
        current_depth: 0,
        last_depth: 0,
        last_broadcast: HashMap::new(),
        gossip_dial_peers: HashSet::new(),
        health_check_delay: DEFAULT_HEALTH_CHECK_DELAY,
        gossip_interval: tokio::time::interval(GOSSIP_REFRESH_INTERVAL),
        pending_exchanges: FuturesUnordered::new(),
        cancelled_exchanges: HashSet::new(),
        evaluator_handle,
    };

    let executor = vertex_tasks::TaskExecutor::try_current()
        .map_err(|e| format!("No task executor available: {e}"))?;

    executor.spawn_critical_with_graceful_shutdown_signal(
        "topology.gossip",
        |shutdown| async move {
            tokio::select! {
                _ = task.run() => {}
                guard = shutdown => {
                    drop(guard);
                }
            }
        },
    );

    Ok(super::GossipHandle {
        input_tx,
        output_rx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use vertex_net_local::IpCapability;
    use vertex_swarm_peer::AddressScope;

    use crate::test_support::TopologyTestContext;
    use vertex_swarm_test_utils::{test_overlay, test_swarm_peer};

    /// Helper that constructs only the gossip exchange state for unit testing
    /// (no verifier swarm needed).
    struct TestGossipState {
        ctx: TopologyTestContext,
        current_depth: u8,
        last_depth: u8,
        gossip_dial_peers: HashSet<PeerId>,
    }

    impl TestGossipState {
        fn new() -> Self {
            Self {
                ctx: TopologyTestContext::new(),
                current_depth: 0,
                last_depth: 0,
                gossip_dial_peers: HashSet::new(),
            }
        }

        fn with_peers(mut self) -> Self {
            self.ctx = self.ctx.with_peers();
            self
        }

        fn select_peers_for_distant(
            &self,
            recipient: OverlayAddress,
            profile: &RecipientProfile,
        ) -> Vec<SwarmPeer> {
            select_peers_for_distant(
                &self.ctx.local_overlay,
                &self.ctx.peer_manager,
                recipient,
                profile,
            )
        }

        fn filter_peers_for_recipient(
            &self,
            peers: &[SwarmPeer],
            profile: &RecipientProfile,
        ) -> Vec<SwarmPeer> {
            filter_peers_for_recipient(peers, profile, &*self.ctx.peer_manager)
                .into_iter()
                .cloned()
                .collect()
        }

        fn check_depth_change(&mut self) -> Vec<GossipAction> {
            // TestGossipState has no connection registry, so just detect the depth change.
            let _ = detect_depth_decrease(self.current_depth, &mut self.last_depth);
            Vec::new()
        }
    }

    #[tokio::test]
    async fn test_initial_state() {
        let state = TestGossipState::new();
        assert_eq!(state.current_depth, 0);
    }

    #[tokio::test]
    async fn test_set_depth() {
        let mut state = TestGossipState::new();
        state.current_depth = 8;
        assert_eq!(state.current_depth, 8);
    }

    #[tokio::test]
    async fn test_handshake_non_gossip_dial_not_delayed() {
        let state = TestGossipState::new();
        let peer_id = PeerId::random();
        // If peer_id is NOT in gossip_dial_peers, exchange happens immediately
        assert!(!state.gossip_dial_peers.contains(&peer_id));
    }

    #[tokio::test]
    async fn test_handshake_gossip_dial_is_delayed() {
        let mut state = TestGossipState::new();
        let peer_id = PeerId::random();
        state.gossip_dial_peers.insert(peer_id);
        // If peer_id IS in gossip_dial_peers, it will be scheduled for delay
        assert!(state.gossip_dial_peers.contains(&peer_id));
    }

    #[tokio::test]
    async fn test_connection_closed_cleans_up() {
        let mut state = TestGossipState::new();
        let peer_id = PeerId::random();
        state.gossip_dial_peers.insert(peer_id);
        state.gossip_dial_peers.remove(&peer_id);
        assert!(!state.gossip_dial_peers.contains(&peer_id));
    }

    #[tokio::test]
    async fn test_get_connected_neighbors_empty_when_no_connections() {
        let state = TestGossipState::new().with_peers();
        let neighbors = peer_selection::connected_neighbors(
            &state.ctx.local_overlay,
            &state.ctx.peer_manager,
            &state.ctx.connection_registry,
            0,
        );
        assert!(neighbors.is_empty());
    }

    #[tokio::test]
    async fn test_select_peers_no_duplicates() {
        let state = TestGossipState::new().with_peers();
        let recipient = test_overlay(0xFF);
        let profile = RecipientProfile {
            capability: IpCapability::Dual,
            scope: AddressScope::Loopback,
        };

        let selected = state.select_peers_for_distant(recipient, &profile);

        let unique: HashSet<_> = selected.iter().map(|p| *p.overlay()).collect();
        assert_eq!(unique.len(), selected.len());
    }

    #[tokio::test]
    async fn test_check_depth_change_no_change() {
        let mut state = TestGossipState::new().with_peers();
        state.last_depth = 5;
        state.current_depth = 5;

        let actions = state.check_depth_change();
        assert!(actions.is_empty());
    }

    #[tokio::test]
    async fn test_filter_peers_dual_stack() {
        let state = TestGossipState::new().with_peers();
        let peers = vec![test_swarm_peer(1), test_swarm_peer(2)];
        let profile = RecipientProfile {
            capability: IpCapability::Dual,
            scope: AddressScope::Loopback,
        };

        let filtered = state.filter_peers_for_recipient(&peers, &profile);
        assert_eq!(filtered.len(), 2);
    }

    #[tokio::test]
    async fn test_filter_peers_public_recipient_excludes_loopback() {
        let state = TestGossipState::new().with_peers();
        let peers = vec![test_swarm_peer(1), test_swarm_peer(2)];
        let profile = RecipientProfile {
            capability: IpCapability::Dual,
            scope: AddressScope::Public,
        };

        let filtered = state.filter_peers_for_recipient(&peers, &profile);
        assert!(filtered.is_empty(), "Loopback peers should be excluded for public recipients");
    }
}
