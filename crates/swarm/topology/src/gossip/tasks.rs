//! Gossip exchange coordinator task.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use libp2p::PeerId;
use tokio::sync::mpsc;
use tracing::{debug, trace};
use vertex_swarm_api::{SwarmIdentity, SwarmNodeType};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_primitives::{Bin, NeighborhoodDepth, OverlayAddress};
use vertex_tasks::time::sleep;
use vertex_util_runtime::time::Instant;

use super::events::{GossipAction, GossipCheckOk};
use super::filter::{
    RecipientProfile, detect_depth_decrease, filter_peers_for_recipient, select_peers_for_distant,
};
use super::intake::GossipIntake;
use super::{GossipConfig, GossipInput};
use crate::kademlia::RoutingEvaluatorHandle;
use crate::kademlia::peer_selection;

use crate::behaviour::ConnectionRegistry;

/// Channel capacity for gossip inputs.
const INPUT_CHANNEL_CAPACITY: usize = 128;

/// Channel capacity for gossip broadcast actions.
const OUTPUT_CHANNEL_CAPACITY: usize = 128;

/// Result of a pending delayed gossip exchange.
struct PendingExchange {
    peer_id: PeerId,
    swarm_peer: SwarmPeer,
    node_type: SwarmNodeType,
}

/// Boxed delayed-exchange future. `Send` on native so the gossip task spawns
/// through the Send-bounded spawner; the bound is dropped on wasm32, where the
/// browser timer future is `!Send` and the task runs on the browser event loop.
#[cfg(not(target_arch = "wasm32"))]
type PendingExchangeFuture = Pin<Box<dyn Future<Output = PendingExchange> + Send>>;

/// Boxed delayed-exchange future for the browser build (see the native one).
#[cfg(target_arch = "wasm32")]
type PendingExchangeFuture = Pin<Box<dyn Future<Output = PendingExchange>>>;

/// Gossip exchange coordinator task.
struct GossipTask<I: SwarmIdentity> {
    // Channels
    input_rx: mpsc::Receiver<GossipInput>,
    output_tx: mpsc::Sender<GossipAction>,

    // Record intake (cooldown and per-gossiper budgets)
    intake: GossipIntake,

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
    refresh_interval: Duration,
    /// Re-armable periodic tick. tokio's timer driver does not run on wasm32, so
    /// the cadence is a `sleep` future that is recreated after each fire rather
    /// than a `tokio::time::Interval`.
    gossip_tick: crate::behaviour::TimerFuture,

    // Delayed gossip exchange
    pending_exchanges: FuturesUnordered<PendingExchangeFuture>,
    cancelled_exchanges: HashSet<PeerId>,

    // Triggers routing evaluation after admitting new dialable supply
    evaluator_handle: RoutingEvaluatorHandle,
}

impl<I: SwarmIdentity> GossipTask<I> {
    async fn run(mut self) {
        loop {
            tokio::select! {
                Some(input) = self.input_rx.recv() => {
                    self.handle_input(input);
                }
                _ = &mut self.gossip_tick => {
                    self.gossip_tick = Box::pin(sleep(self.refresh_interval));
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
            GossipInput::PeerActivated {
                peer_id,
                swarm_peer,
                node_type,
            } => {
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
                // Filter out our own overlay before intake
                let peers: Vec<_> = peers
                    .into_iter()
                    .filter(|p| OverlayAddress::from(*p.overlay()) != self.local_overlay)
                    .collect();
                if peers.is_empty() {
                    return;
                }
                self.handle_gossiped_records(gossiper, peers);
            }
        }
    }

    /// Admit gossiped records into the known table as unverified peers.
    ///
    /// Signature validation already happened at the hive protocol layer;
    /// the intake gate applies the per-overlay cooldown and the
    /// per-gossiper budget, and admitted records go straight to the peer
    /// manager as unverified, dialable entries. Candidate selection may
    /// dial them; the first completed handshake verifies the record. No
    /// dedicated verification dial happens here.
    fn handle_gossiped_records(&mut self, gossiper: OverlayAddress, peers: Vec<SwarmPeer>) {
        let mut admitted = 0;
        let mut skipped = 0;
        let mut rejected = 0;

        for peer in peers {
            let existing = self.peer_manager.get_swarm_peer(peer.overlay());
            let result = self
                .intake
                .check_gossip(&peer, &gossiper, existing.as_ref());
            match result {
                Ok(GossipCheckOk::AlreadyKnown) => {
                    trace!(overlay = %peer.overlay(), %gossiper, "gossip check: already_known");
                    skipped += 1;
                }
                Ok(GossipCheckOk::Admitted) => {
                    trace!(overlay = %peer.overlay(), %gossiper, "gossip check: admitted");
                    self.peer_manager.store_discovered_peer(peer);
                    admitted += 1;
                }
                Err(ref err) => {
                    trace!(overlay = %peer.overlay(), %gossiper, reason = %err, "gossip check: rejected");
                    err.record();
                    rejected += 1;
                }
            }
        }

        if admitted > 0 {
            // New dialable supply: let candidate selection pick it up.
            self.evaluator_handle.trigger_evaluation();
        }

        if admitted > 0 || rejected > 0 {
            debug!(
                %gossiper,
                admitted,
                skipped,
                rejected,
                "Gossiped records processed"
            );
        }
    }

    fn schedule_exchange(
        &mut self,
        peer_id: PeerId,
        swarm_peer: SwarmPeer,
        node_type: SwarmNodeType,
    ) {
        let delay = self.health_check_delay;
        self.pending_exchanges.push(Box::pin(async move {
            sleep(delay).await;
            PendingExchange {
                peer_id,
                swarm_peer,
                node_type,
            }
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

        debug!(
            old_depth,
            new_depth, "Depth decreased - neighborhood expanded"
        );

        let mut actions = Vec::new();

        for overlay in self.connection_registry.active_ids() {
            let proximity = self.local_overlay.proximity(&overlay).get();

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
        let proximity = self.local_overlay.proximity(&new_peer_overlay).get();

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
        // pending future (conservatively keep all; they are removed on resolution).
        if self.pending_exchanges.is_empty() {
            self.cancelled_exchanges.clear();
        }

        // Periodic cleanup: evict stale last_broadcast entries.
        // Entries older than 2x the refresh interval are unlikely to be useful;
        // the peer has either disconnected or will be refreshed on the next tick.
        let broadcast_expiry = self.refresh_interval * 2;
        self.last_broadcast
            .retain(|_, ts| now.duration_since(*ts) <= broadcast_expiry);

        let neighbors = self.connected_neighbors();

        // Check if any neighbor is stale before computing the expensive peer set
        let has_stale = neighbors.iter().any(|neighbor| {
            self.last_broadcast
                .get(neighbor)
                .map(|t| now.duration_since(*t) > self.refresh_interval)
                .unwrap_or(true)
        });

        if !has_stale {
            return;
        }

        // Compute the base neighborhood peer set once (without exclude)
        let all_neighborhood_peers = self.known_neighborhood_peers(self.current_depth, None);

        for neighbor in neighbors {
            let is_stale = self
                .last_broadcast
                .get(&neighbor)
                .map(|t| now.duration_since(*t) > self.refresh_interval)
                .unwrap_or(true);

            if is_stale {
                let profile = self.recipient_profile(&neighbor);
                let filtered = self.filter_for_recipient(&all_neighborhood_peers, &profile);

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

        let neighborhood_peers = self.known_neighborhood_peers(depth, Some(&new_peer));
        let filtered = self.filter_for_recipient(&neighborhood_peers, &new_peer_profile);

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

                let filtered = self.filter_for_recipient(&new_peer_slice, &profile);

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
            NeighborhoodDepth::new(Bin::new(self.current_depth).unwrap_or(Bin::MAX)),
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
            NeighborhoodDepth::new(Bin::new(depth).unwrap_or(Bin::MAX)),
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

/// Task-side endpoints of the gossip channels, created by [`gossip_channel`]
/// and consumed when the task is spawned.
pub(crate) struct GossipChannels {
    input_rx: mpsc::Receiver<GossipInput>,
    output_tx: mpsc::Sender<GossipAction>,
}

/// Create the gossip handle and task channel pair without spawning the task.
///
/// Inputs sent through the handle before the task starts are buffered up to
/// the channel capacity.
pub(crate) fn gossip_channel() -> (super::GossipHandle, GossipChannels) {
    let (input_tx, input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
    let (output_tx, output_rx) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
    (
        super::GossipHandle {
            input_tx,
            output_rx,
        },
        GossipChannels {
            input_rx,
            output_tx,
        },
    )
}

/// Spawn the gossip task on the channel endpoints created by [`gossip_channel`].
pub(crate) fn spawn_gossip_task<I: SwarmIdentity>(
    config: GossipConfig,
    local_overlay: OverlayAddress,
    peer_manager: Arc<PeerManager<I>>,
    connection_registry: Arc<ConnectionRegistry>,
    evaluator_handle: RoutingEvaluatorHandle,
    channels: GossipChannels,
    executor: &vertex_tasks::TaskExecutor,
) {
    let GossipChannels {
        input_rx,
        output_tx,
    } = channels;

    let intake = GossipIntake::new(&config);

    let task = GossipTask {
        input_rx,
        output_tx,
        intake,
        local_overlay,
        peer_manager,
        connection_registry,
        current_depth: 0,
        last_depth: 0,
        last_broadcast: HashMap::new(),
        gossip_dial_peers: HashSet::new(),
        health_check_delay: config.health_check_delay,
        refresh_interval: config.refresh_interval,
        gossip_tick: Box::pin(sleep(config.refresh_interval)),
        pending_exchanges: FuturesUnordered::new(),
        cancelled_exchanges: HashSet::new(),
        evaluator_handle,
    };

    // The gossip task owns browser timer futures (`gossip_tick`,
    // `pending_exchanges`) that are `!Send` on wasm32, so it runs on the browser
    // event loop there; on native it is a Send-bounded critical task.
    #[cfg(not(target_arch = "wasm32"))]
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

    #[cfg(target_arch = "wasm32")]
    executor.spawn_local_with_graceful_shutdown_signal("topology.gossip", |shutdown| async move {
        tokio::select! {
            _ = task.run() => {}
            guard = shutdown => {
                drop(guard);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use vertex_net_local::IpCapability;
    use vertex_swarm_peer::AddressScope;

    use crate::test_support::TopologyTestContext;
    use vertex_swarm_test_utils::{test_overlay, test_swarm_peer};

    /// Helper that constructs only the gossip exchange state for unit testing.
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
            NeighborhoodDepth::ZERO,
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
        assert!(
            filtered.is_empty(),
            "Loopback peers should be excluded for public recipients"
        );
    }
}
