//! Synchronous gossip engine owned by the topology behaviour.
//!
//! Every entry point is a plain method returning the [`GossipAction`]s to
//! broadcast, applied by the caller in the same call, so gossip can neither
//! lag behind the events that drive it nor drop under queue pressure. The two
//! time-driven paths (the periodic neighbourhood refresh and the delayed
//! exchange after a gossip dial) are polled from the behaviour's `poll` like
//! its other timers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use libp2p::PeerId;
use tracing::{debug, trace};
use vertex_swarm_api::{SwarmIdentity, SwarmNodeType};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_primitives::{Bin, NeighborhoodDepth, OverlayAddress};
use vertex_util_runtime::time::Instant;

use super::GossipConfig;
use super::events::{GossipAction, GossipCheckOk};
use super::filter::{
    RecipientProfile, detect_depth_decrease, filter_peers_for_recipient, select_peers_for_distant,
};
use super::intake::GossipIntake;
use crate::kademlia::RoutingEvaluatorHandle;
use crate::kademlia::peer_selection;

use crate::behaviour::ConnectionRegistry;

/// A gossip exchange deferred until its connection proves stable.
struct PendingExchange {
    deadline: Instant,
    peer_id: PeerId,
    swarm_peer: SwarmPeer,
    node_type: SwarmNodeType,
}

/// Synchronous gossip engine: peer exchange policy and record intake.
pub(crate) struct GossipEngine<I: SwarmIdentity> {
    // Record intake (cooldown and per-gossiper budgets)
    intake: GossipIntake,

    // Gossip state
    local_overlay: OverlayAddress,
    peer_manager: Arc<PeerManager<I>>,
    connection_registry: Arc<ConnectionRegistry>,
    /// The published depth last seen by the decrease detector.
    last_depth: u8,
    last_broadcast: HashMap<OverlayAddress, Instant>,
    /// Peers we initiated a gossip-dial to. Bounded by active outbound
    /// connections: entries are added once per outbound discovery dial and
    /// removed on activation (connection succeeded) or connection close.
    gossip_dial_peers: HashSet<PeerId>,
    health_check_delay: Duration,
    refresh_interval: Duration,
    /// Connected storers per bin told about a newly connected distant storer.
    broadcast_bin_size: usize,
    /// Periodic refresh tick; first fire one full period after construction.
    gossip_tick: vertex_tasks::time::Interval,

    /// Deadline-ordered pending exchanges plus one timer armed for the
    /// earliest deadline; cancellation is a `retain`, so no cancellation set
    /// exists.
    pending_exchanges: Vec<PendingExchange>,
    exchange_timer: Option<vertex_tasks::time::BoxTimerFuture>,

    // Triggers routing evaluation after admitting new dialable supply
    evaluator_handle: RoutingEvaluatorHandle,
}

impl<I: SwarmIdentity> GossipEngine<I> {
    pub(crate) fn new(
        config: GossipConfig,
        local_overlay: OverlayAddress,
        peer_manager: Arc<PeerManager<I>>,
        connection_registry: Arc<ConnectionRegistry>,
        evaluator_handle: RoutingEvaluatorHandle,
    ) -> Self {
        Self {
            intake: GossipIntake::new(&config),
            local_overlay,
            peer_manager,
            connection_registry,
            last_depth: 0,
            last_broadcast: HashMap::new(),
            gossip_dial_peers: HashSet::new(),
            health_check_delay: config.health_check_delay,
            refresh_interval: config.refresh_interval,
            broadcast_bin_size: config.broadcast_bin_size,
            gossip_tick: vertex_tasks::time::interval_after(
                config.refresh_interval,
                config.refresh_interval,
            ),
            pending_exchanges: Vec::new(),
            exchange_timer: None,
            evaluator_handle,
        }
    }

    /// Drive the two time-based paths: the periodic neighbourhood refresh and
    /// due delayed exchanges. Registers wakers through the interval and the
    /// deadline timer, so the behaviour wakes exactly when gossip has work.
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, depth: u8) -> Vec<GossipAction> {
        let mut actions = Vec::new();

        if self.gossip_tick.poll_tick(cx).is_ready() {
            actions.extend(self.on_tick(depth));
        }

        // Loop so a re-armed timer is polled once and registers its waker.
        while let Some(timer) = self.exchange_timer.as_mut() {
            match timer.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    self.exchange_timer = None;
                    actions.extend(self.drain_due_exchanges(depth));
                    self.arm_exchange_timer();
                }
                Poll::Pending => break,
            }
        }

        actions
    }

    /// Mark an outbound discovery dial so its exchange is deferred on
    /// activation (the peer may drop us if its bin is saturated).
    pub(crate) fn mark_gossip_dial(&mut self, peer_id: PeerId) {
        self.gossip_dial_peers.insert(peer_id);
    }

    /// A peer completed activation: exchange immediately, or after the
    /// health-check delay when we gossip-dialed it.
    pub(crate) fn on_peer_activated(
        &mut self,
        peer_id: PeerId,
        swarm_peer: SwarmPeer,
        node_type: SwarmNodeType,
        depth: u8,
    ) -> Vec<GossipAction> {
        if self.gossip_dial_peers.remove(&peer_id) {
            self.schedule_exchange(peer_id, swarm_peer, node_type);
            Vec::new()
        } else {
            self.exchange_gossip(&swarm_peer, node_type, depth)
        }
    }

    /// A connection closed: drop its dial mark, cancel any deferred exchange,
    /// forget its broadcast stamp, and gossip a depth decrease if one landed.
    pub(crate) fn on_connection_closed(
        &mut self,
        peer_id: PeerId,
        overlay: Option<OverlayAddress>,
        depth: u8,
    ) -> Vec<GossipAction> {
        self.gossip_dial_peers.remove(&peer_id);
        let before = self.pending_exchanges.len();
        self.pending_exchanges.retain(|p| p.peer_id != peer_id);
        if self.pending_exchanges.len() != before {
            self.arm_exchange_timer();
        }
        if let Some(overlay) = &overlay {
            self.last_broadcast.remove(overlay);
        }
        self.check_depth_change(depth)
    }

    /// The published depth changed: gossip newly promoted neighbours on a
    /// decrease.
    pub(crate) fn on_depth_changed(&mut self, depth: u8) -> Vec<GossipAction> {
        self.check_depth_change(depth)
    }

    /// Admit gossiped records into the known table as unverified peers.
    ///
    /// Signature validation already happened at the hive protocol layer;
    /// the intake gate applies the per-overlay cooldown and the
    /// per-gossiper budget, and admitted records go straight to the peer
    /// manager as unverified, dialable entries. Candidate selection may
    /// dial them; the first completed handshake verifies the record. No
    /// dedicated verification dial happens here.
    pub(crate) fn on_peers_received(&mut self, gossiper: OverlayAddress, peers: Vec<SwarmPeer>) {
        let peers: Vec<_> = peers
            .into_iter()
            .filter(|p| OverlayAddress::from(*p.overlay()) != self.local_overlay)
            .collect();
        if peers.is_empty() {
            return;
        }

        let mut admitted = 0;
        let mut skipped = 0;
        let mut rejected = 0;

        for peer in peers {
            let existing = self.peer_manager.swarm_peer(peer.overlay());
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

    // Delayed exchanges

    fn schedule_exchange(
        &mut self,
        peer_id: PeerId,
        swarm_peer: SwarmPeer,
        node_type: SwarmNodeType,
    ) {
        self.pending_exchanges.push(PendingExchange {
            deadline: Instant::now() + self.health_check_delay,
            peer_id,
            swarm_peer,
            node_type,
        });
        self.arm_exchange_timer();
    }

    fn drain_due_exchanges(&mut self, depth: u8) -> Vec<GossipAction> {
        let now = Instant::now();
        let mut due = Vec::new();
        self.pending_exchanges.retain(|p| {
            if p.deadline <= now {
                due.push((p.swarm_peer.clone(), p.node_type));
                false
            } else {
                true
            }
        });
        let mut actions = Vec::new();
        for (swarm_peer, node_type) in due {
            actions.extend(self.exchange_gossip(&swarm_peer, node_type, depth));
        }
        actions
    }

    /// Arm (or re-arm) the exchange timer for the earliest pending deadline.
    fn arm_exchange_timer(&mut self) {
        let Some(earliest) = self.pending_exchanges.iter().map(|p| p.deadline).min() else {
            self.exchange_timer = None;
            return;
        };
        let delay = earliest.saturating_duration_since(Instant::now());
        self.exchange_timer = Some(Box::pin(vertex_tasks::time::sleep(delay)));
    }

    fn exchange_gossip(
        &mut self,
        swarm_peer: &SwarmPeer,
        node_type: SwarmNodeType,
        depth: u8,
    ) -> Vec<GossipAction> {
        let mut actions = self.on_peer_authenticated(swarm_peer, node_type, depth);
        actions.extend(self.check_depth_change(depth));
        actions
    }

    // Gossip exchange logic

    fn check_depth_change(&mut self, depth: u8) -> Vec<GossipAction> {
        let Some((old_depth, new_depth)) = detect_depth_decrease(depth, &mut self.last_depth)
        else {
            return Vec::new();
        };

        debug!(
            old_depth,
            new_depth, "Depth decreased: neighborhood expanded"
        );

        let mut actions = Vec::new();

        for overlay in self.connection_registry.active_ids() {
            let proximity = self.local_overlay.proximity(&overlay).get();

            if proximity >= new_depth
                && proximity < old_depth
                && self.peer_manager.node_type(&overlay) == Some(SwarmNodeType::Storer)
            {
                debug!(%overlay, proximity, "Peer became neighbor due to depth change");

                if let Some(peer) = self.peer_manager.swarm_peer(&overlay) {
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

        let new_peer_overlay = OverlayAddress::from(*peer.overlay());

        if !node_type.requires_storage() {
            // A connecting client cannot grow past its bootnodes without
            // help: send it the same recipient-targeted bootstrap set a
            // distant storer receives. Clients are recipients only; they are
            // never gossiped about.
            if node_type == SwarmNodeType::Client {
                return self.handle_new_distant_peer(new_peer_overlay);
            }
            trace!(overlay = %peer.overlay(), "Skipping gossip for non-storer node");
            return Vec::new();
        }

        let proximity = self.local_overlay.proximity(&new_peer_overlay).get();

        let mut actions = if proximity >= depth {
            self.handle_new_neighbor(new_peer_overlay, peer.clone(), depth)
        } else {
            // Whether this storer was announced within the window is read
            // before the bootstrap re-stamps its broadcast time, so a rapid
            // reconnect does not re-announce it to the sample.
            let announce = !self.broadcast_within_window(&new_peer_overlay);
            let mut distant = self.handle_new_distant_peer(new_peer_overlay);
            if announce {
                let sample = self.announce_storer_to_sample(peer, &new_peer_overlay);
                if !sample.is_empty() {
                    // Stamp the newcomer so a rapid reconnect skips the sample,
                    // independent of whether the bootstrap already stamped it.
                    self.last_broadcast.insert(new_peer_overlay, Instant::now());
                }
                distant.extend(sample);
            }
            distant
        };
        // Connected clients hear about every newly connected storer, so they
        // keep building topology from live supply, not just their bootstrap.
        actions.extend(self.notify_clients(peer));
        actions
    }

    /// Announce a newly connected distant storer to a bounded per-bin sample
    /// of connected storers, so third parties learn it without having to dial
    /// it first. The newcomer is excluded and every recipient is reachability
    /// filtered, so an unreachable record is never propagated.
    fn announce_storer_to_sample(
        &self,
        new_peer: &SwarmPeer,
        new_overlay: &OverlayAddress,
    ) -> Vec<GossipAction> {
        let new_slice = [new_peer.clone()];
        let mut actions = Vec::new();
        for recipient in self.connected_storer_sample(new_overlay) {
            let profile = self.recipient_profile(&recipient);
            let filtered = self.filter_for_recipient(&new_slice, &profile);
            if !filtered.is_empty() {
                trace!(to = %recipient, about = %new_peer.overlay(), "Announcing new storer to sample");
                actions.push(GossipAction {
                    to: recipient,
                    peers: filtered.into_iter().cloned().collect(),
                });
            }
        }
        actions
    }

    /// True when we last sent gossip to `overlay` within the refresh window.
    fn broadcast_within_window(&self, overlay: &OverlayAddress) -> bool {
        self.last_broadcast
            .get(overlay)
            .is_some_and(|t| t.elapsed() < self.refresh_interval)
    }

    fn connected_storer_sample(&self, subject: &OverlayAddress) -> Vec<OverlayAddress> {
        peer_selection::connected_storer_sample(
            &self.local_overlay,
            &self.peer_manager,
            &self.connection_registry,
            self.broadcast_bin_size,
            // The newcomer is both excluded as a recipient and the anchor the
            // per-bin sample is keyed on, so recipients rotate with the subject.
            subject,
            subject,
        )
    }

    /// Tell every connected client about a newly connected storer.
    ///
    /// Clients receive gossip but never appear in it: the payload here is the
    /// storer, filtered per recipient reachability like any other broadcast.
    fn notify_clients(&self, new_peer_info: &SwarmPeer) -> Vec<GossipAction> {
        let new_peer_slice = [new_peer_info.clone()];
        let mut actions = Vec::new();
        for client in self.connected_clients() {
            let profile = self.recipient_profile(&client);
            let filtered = self.filter_for_recipient(&new_peer_slice, &profile);
            if !filtered.is_empty() {
                trace!(to = %client, about = %new_peer_info.overlay(), "Notifying client about new storer");
                actions.push(GossipAction {
                    to: client,
                    peers: filtered.into_iter().cloned().collect(),
                });
            }
        }
        actions
    }

    fn connected_clients(&self) -> Vec<OverlayAddress> {
        peer_selection::connected_clients(&self.peer_manager, &self.connection_registry)
    }

    fn on_tick(&mut self, depth: u8) -> Vec<GossipAction> {
        let now = Instant::now();
        let mut actions = Vec::new();

        // Periodic cleanup: evict stale last_broadcast entries.
        // Entries older than 2x the refresh interval are unlikely to be useful;
        // the peer has either disconnected or will be refreshed on the next tick.
        let broadcast_expiry = self.refresh_interval * 2;
        self.last_broadcast
            .retain(|_, ts| now.duration_since(*ts) <= broadcast_expiry);

        let neighbors = self.connected_neighbors(depth);

        // Check if any neighbor is stale before computing the expensive peer set
        let has_stale = neighbors.iter().any(|neighbor| {
            self.last_broadcast
                .get(neighbor)
                .map(|t| now.duration_since(*t) > self.refresh_interval)
                .unwrap_or(true)
        });

        if !has_stale {
            return actions;
        }

        // Compute the base neighborhood peer set once (without exclude)
        let all_neighborhood_peers = self.known_neighborhood_peers(depth, None);

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

        actions
    }

    fn handle_new_neighbor(
        &mut self,
        new_peer: OverlayAddress,
        new_peer_info: SwarmPeer,
        depth: u8,
    ) -> Vec<GossipAction> {
        let mut actions = Vec::new();

        debug!(%new_peer, depth, "New neighbor joined: initiating neighborhood exchange");

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

        let existing_neighbors = self.connected_neighbors(depth);

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

    fn connected_neighbors(&self, depth: u8) -> Vec<OverlayAddress> {
        peer_selection::connected_neighbors(
            &self.local_overlay,
            &self.peer_manager,
            &self.connection_registry,
            NeighborhoodDepth::new(Bin::new(depth).unwrap_or(Bin::MAX)),
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

#[cfg(test)]
mod tests {
    use super::*;

    use vertex_net_local::IpCapability;
    use vertex_swarm_peer::AddressScope;
    use vertex_swarm_test_utils::MockIdentity;

    use crate::test_support::TopologyTestContext;
    use vertex_swarm_test_utils::{test_overlay, test_swarm_peer};

    fn test_engine(ctx: &TopologyTestContext) -> GossipEngine<MockIdentity> {
        GossipEngine::new(
            GossipConfig::default(),
            ctx.local_overlay,
            Arc::clone(&ctx.peer_manager),
            Arc::clone(&ctx.connection_registry),
            RoutingEvaluatorHandle::new(),
        )
    }

    /// Register `n` as a known, actively connected client.
    fn connect_client(ctx: &TopologyTestContext, n: u8) -> OverlayAddress {
        let peer = test_swarm_peer(n);
        let overlay = OverlayAddress::from(*peer.overlay());
        ctx.peer_manager.on_peer_connected(
            peer,
            SwarmNodeType::Client,
            vertex_net_peer_registry::ConnectionDirection::Inbound,
            vertex_swarm_peer_manager::TrustLevel::Normal,
        );
        let peer_id = PeerId::random();
        let connection_id = libp2p::swarm::ConnectionId::new_unchecked(usize::from(n));
        ctx.connection_registry
            .connected_inbound(peer_id, connection_id);
        ctx.connection_registry
            .activate(peer_id, connection_id, overlay);
        overlay
    }

    /// Register `n` as a known, actively connected storer (a candidate for the
    /// announcement sample).
    fn connect_storer(ctx: &TopologyTestContext, n: u8) -> OverlayAddress {
        let peer = test_swarm_peer(n);
        let overlay = OverlayAddress::from(*peer.overlay());
        ctx.peer_manager.on_peer_connected(
            peer,
            SwarmNodeType::Storer,
            vertex_net_peer_registry::ConnectionDirection::Outbound,
            vertex_swarm_peer_manager::TrustLevel::Normal,
        );
        let peer_id = PeerId::random();
        let connection_id = libp2p::swarm::ConnectionId::new_unchecked(usize::from(n));
        ctx.connection_registry
            .connected_inbound(peer_id, connection_id);
        ctx.connection_registry
            .activate(peer_id, connection_id, overlay);
        overlay
    }

    #[test]
    fn new_distant_storer_is_announced_to_a_per_bin_sample() {
        // local overlay is byte 0, so proximity to a peer is the leading zero
        // bits of its first byte: 0x40 sits in bin 1, 0x20 in bin 2, and the
        // newcomer 0x80 in bin 0. At depth 3 the newcomer is distant, so the
        // announcement path (not the neighbour path) runs.
        let ctx = TopologyTestContext::new();
        let mut engine = test_engine(&ctx);
        let bin1 = connect_storer(&ctx, 0x40);
        let bin2 = connect_storer(&ctx, 0x20);

        let newcomer = test_swarm_peer(0x80);
        let newcomer_overlay = OverlayAddress::from(*newcomer.overlay());
        let actions = engine.on_peer_authenticated(&newcomer, SwarmNodeType::Storer, 3);

        for recipient in [bin1, bin2] {
            let notify = actions
                .iter()
                .find(|a| a.to == recipient)
                .expect("a connected storer in the sample is told about the newcomer");
            assert_eq!(notify.peers.len(), 1);
            assert_eq!(
                notify.peers.first().expect("one announced peer").overlay(),
                newcomer.overlay()
            );
        }
        assert!(
            actions.iter().all(|a| a.to != newcomer_overlay
                || a.peers.iter().all(|p| p.overlay() != newcomer.overlay())),
            "the newcomer is never announced to itself"
        );
    }

    #[test]
    fn a_reconnecting_storer_is_not_re_announced_within_the_window() {
        let ctx = TopologyTestContext::new();
        let mut engine = test_engine(&ctx);
        let recipient = connect_storer(&ctx, 0x40);

        let newcomer = test_swarm_peer(0x80);
        let first = engine.on_peer_authenticated(&newcomer, SwarmNodeType::Storer, 3);
        assert!(
            first.iter().any(|a| a.to == recipient),
            "the first connect announces the newcomer"
        );

        let second = engine.on_peer_authenticated(&newcomer, SwarmNodeType::Storer, 3);
        assert!(
            second.iter().all(|a| a.to != recipient),
            "a reconnect within the window does not re-announce it to the sample"
        );
    }

    #[test]
    fn non_gossip_dial_exchanges_immediately() {
        let ctx = TopologyTestContext::new().with_peers();
        let mut engine = test_engine(&ctx);
        // A connected client observes the exchange: the new storer is
        // announced to it in the same call.
        connect_client(&ctx, 0xC9);

        let storer = test_swarm_peer(0xD0);
        let actions = engine.on_peer_activated(PeerId::random(), storer, SwarmNodeType::Storer, 0);

        assert!(
            !actions.is_empty(),
            "an unmarked activation exchanges in the same call"
        );
        assert!(engine.pending_exchanges.is_empty());
    }

    #[tokio::test]
    async fn gossip_dial_defers_the_exchange_until_its_deadline() {
        let ctx = TopologyTestContext::new().with_peers();
        let mut engine = test_engine(&ctx);
        let peer_id = PeerId::random();

        engine.mark_gossip_dial(peer_id);
        let actions =
            engine.on_peer_activated(peer_id, test_swarm_peer(0xD0), SwarmNodeType::Storer, 0);

        assert!(actions.is_empty(), "a marked activation defers");
        assert_eq!(engine.pending_exchanges.len(), 1);
        assert!(
            engine.exchange_timer.is_some(),
            "the deadline timer is armed"
        );
    }

    #[tokio::test]
    async fn connection_close_cancels_the_deferred_exchange() {
        let ctx = TopologyTestContext::new().with_peers();
        let mut engine = test_engine(&ctx);
        let peer_id = PeerId::random();

        engine.mark_gossip_dial(peer_id);
        engine.on_peer_activated(peer_id, test_swarm_peer(0xD0), SwarmNodeType::Storer, 0);
        engine.on_connection_closed(peer_id, None, 0);

        assert!(
            engine.pending_exchanges.is_empty(),
            "cancellation is a retain"
        );
        assert!(
            engine.exchange_timer.is_none(),
            "no deadline left to arm for"
        );
    }

    #[tokio::test]
    async fn due_exchanges_drain_and_disarm() {
        let ctx = TopologyTestContext::new().with_peers();
        let mut engine = test_engine(&ctx);
        engine.health_check_delay = Duration::ZERO;
        // A connected client observes the drained exchange.
        connect_client(&ctx, 0xC8);
        let peer_id = PeerId::random();

        engine.mark_gossip_dial(peer_id);
        engine.on_peer_activated(peer_id, test_swarm_peer(0xD0), SwarmNodeType::Storer, 0);
        let actions = engine.drain_due_exchanges(0);

        assert!(!actions.is_empty(), "the due exchange runs");
        assert!(engine.pending_exchanges.is_empty());
    }

    #[test]
    fn depth_decrease_is_detected_once() {
        let ctx = TopologyTestContext::new().with_peers();
        let mut engine = test_engine(&ctx);
        engine.last_depth = 5;

        // No connected peers, so no promotion actions; the detector still
        // latches so a second call at the same depth is a no-op.
        let _ = engine.on_depth_changed(3);
        assert_eq!(engine.last_depth, 3);
        assert!(engine.on_depth_changed(3).is_empty());
    }

    #[test]
    fn select_peers_no_duplicates() {
        let ctx = TopologyTestContext::new().with_peers();
        let engine = test_engine(&ctx);
        let recipient = test_overlay(0xFF);
        let profile = RecipientProfile {
            capability: IpCapability::Dual,
            scope: AddressScope::Loopback,
        };

        let selected = engine.select_for_distant(recipient, &profile);

        let unique: HashSet<_> = selected.iter().map(|p| *p.overlay()).collect();
        assert_eq!(unique.len(), selected.len());
    }

    #[test]
    fn filter_peers_dual_stack() {
        let ctx = TopologyTestContext::new().with_peers();
        let engine = test_engine(&ctx);
        let peers = vec![test_swarm_peer(1), test_swarm_peer(2)];
        let profile = RecipientProfile {
            capability: IpCapability::Dual,
            scope: AddressScope::Loopback,
        };

        let filtered = engine.filter_for_recipient(&peers, &profile);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_peers_public_recipient_excludes_loopback() {
        let ctx = TopologyTestContext::new().with_peers();
        let engine = test_engine(&ctx);
        let peers = vec![test_swarm_peer(1), test_swarm_peer(2)];
        let profile = RecipientProfile {
            capability: IpCapability::Dual,
            scope: AddressScope::Public,
        };

        let filtered = engine.filter_for_recipient(&peers, &profile);
        assert!(
            filtered.is_empty(),
            "Loopback peers should be excluded for public recipients"
        );
    }

    mod client_recipients {
        use super::*;

        #[test]
        fn connecting_client_receives_bootstrap_peers() {
            let ctx = TopologyTestContext::new().with_peers();
            let mut engine = test_engine(&ctx);

            let client = test_swarm_peer(0xC1);
            let client_overlay = OverlayAddress::from(*client.overlay());
            ctx.peer_manager.on_peer_connected(
                client.clone(),
                SwarmNodeType::Client,
                vertex_net_peer_registry::ConnectionDirection::Inbound,
                vertex_swarm_peer_manager::TrustLevel::Normal,
            );

            let actions = engine.on_peer_authenticated(&client, SwarmNodeType::Client, 0);

            let action = actions.first().expect("one bootstrap action");
            assert_eq!(actions.len(), 1);
            assert_eq!(action.to, client_overlay);
            assert!(!action.peers.is_empty(), "the client receives a peer list");
            assert!(
                action.peers.iter().all(|p| {
                    ctx.peer_manager
                        .node_type(&OverlayAddress::from(*p.overlay()))
                        == Some(SwarmNodeType::Storer)
                }),
                "the payload carries storers only"
            );
        }

        #[test]
        fn new_storer_is_announced_to_connected_clients() {
            let ctx = TopologyTestContext::new().with_peers();
            let mut engine = test_engine(&ctx);
            let client_overlay = connect_client(&ctx, 0xC2);

            let storer = test_swarm_peer(0xD1);
            let actions = engine.on_peer_authenticated(&storer, SwarmNodeType::Storer, 0);

            let notify = actions
                .iter()
                .find(|a| a.to == client_overlay)
                .expect("the connected client is notified");
            let announced = notify.peers.first().expect("one announced peer");
            assert_eq!(notify.peers.len(), 1);
            assert_eq!(announced.overlay(), storer.overlay());
        }

        #[test]
        fn clients_never_appear_in_gossip_payloads() {
            let ctx = TopologyTestContext::new().with_peers();
            let mut engine = test_engine(&ctx);
            let client_overlay = connect_client(&ctx, 0xC3);

            let storer = test_swarm_peer(0xD2);
            let actions = engine.on_peer_authenticated(&storer, SwarmNodeType::Storer, 0);

            assert!(!actions.is_empty());
            assert!(
                actions.iter().all(|a| a
                    .peers
                    .iter()
                    .all(|p| OverlayAddress::from(*p.overlay()) != client_overlay)),
                "clients are recipients only, never subjects"
            );
        }
    }
}
