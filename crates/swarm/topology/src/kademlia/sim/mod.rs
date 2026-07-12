//! Deterministic, transport-free simulation harness for the routing evaluation
//! loop.
//!
//! [`SimWorld`] drives the real [`KademliaRouting`] plus [`PeerManager`] through
//! scripted adversarial populations with no libp2p and no background tasks: each
//! tick evaluates candidates, resolves dials synchronously against a per-peer
//! [`PeerScript`], applies scripted churn, refreshes depth, and checks the
//! routing invariants. Selection is rng-free; the only randomness is the seeded
//! churn ordering.
//!
//! Three clocks coexist and only tokio's is pausable, so scenarios assert
//! exclusion and holding (a locked-out peer is never reselected), never
//! wall-clock expiry of the phase window or the dial backoff.

#![allow(clippy::indexing_slicing)]

mod scenarios;

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::{Address, Signature, U256};
use nectar_primitives::{XorMetric, recompute_neighborhood_depth};
use rand::{SeedableRng, rngs::StdRng, seq::SliceRandom};
use vertex_net_peer_registry::ConnectionDirection;
use vertex_swarm_api::{DisconnectReason, SwarmIdentity, SwarmSpec};
use vertex_swarm_peer::{SwarmPeer, Timestamp};
use vertex_swarm_peer_manager::{PeerManager, PeerManagerConfig, TrustLevel};
use vertex_swarm_primitives::{
    Bin, NeighborhoodDepth, Nonce, OverlayAddress, SwarmNodeType, all_bins,
};
use vertex_swarm_test_utils::MockIdentity;

use super::{KademliaConfig, KademliaRouting, RoutingCapacity, SwarmRouting};
use crate::test_support::{overlay_in_bin, overlay_in_bin_with_slot};

/// Scripted per-peer behaviour the dial resolver applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeerScript {
    /// Reachable: the dial completes the handshake and the peer stays connected.
    Honest,
    /// Every dial fails, arming the peer's wall-clock backoff.
    Unreachable,
    /// Connects like `Honest`, then disconnects cleanly `after_ticks` ticks
    /// later (re-dialable, no backoff).
    AcceptThenDrop { after_ticks: usize },
    /// The dial's transport connects but the handshake fails on the next
    /// tick, releasing the handshake-phase reservation and arming backoff.
    /// The one-tick hold lets the reconciliation observe the held phase.
    HandshakeFail,
    /// Reachable like `Honest`; the adversarial trait is address-space
    /// clustering, set up by the caller's forged overlays.
    Sybil,
}

/// A scripted member of the simulated population.
#[derive(Clone, Copy)]
struct SimPeer {
    node_type: SwarmNodeType,
    script: PeerScript,
}

/// The sim's shadow view of a peer's connection phase, cross-checked every tick
/// against the routing table's own per-bin counters.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SimPhase {
    Dialing,
    Handshaking(ConnectionDirection),
    Active(ConnectionDirection),
}

impl SimPhase {
    /// Whether this phase is an outbound (self-dialed) connection: dialing is
    /// outbound by construction, the later phases by their carried direction.
    fn is_outbound(&self) -> bool {
        match self {
            SimPhase::Dialing => true,
            SimPhase::Handshaking(dir) | SimPhase::Active(dir) => dir.is_outbound(),
        }
    }
}

/// Forge a gossip/handshake peer record for `overlay` stamped at `ts`.
///
/// Timestamps are forged monotonically by the caller so re-discovery is never
/// rejected as stale gossip.
fn forge_peer(overlay: OverlayAddress, ts: i64) -> SwarmPeer {
    SwarmPeer::from_parts(
        Vec::new(),
        Signature::new(U256::ZERO, U256::ZERO, false),
        overlay,
        Nonce::ZERO,
        Timestamp::from_seconds(ts),
        None,
        Address::ZERO,
    )
}

/// A deterministic world driving the real evaluation loop.
struct SimWorld {
    base: OverlayAddress,
    routing: Arc<KademliaRouting<MockIdentity>>,
    peer_manager: Arc<PeerManager<MockIdentity>>,
    population: HashMap<OverlayAddress, SimPeer>,
    /// Shadow phase per peer; the invariant checker reconciles it against the
    /// routing table's per-bin phase counters.
    phases: HashMap<OverlayAddress, SimPhase>,
    /// Tick at which a peer became active, for `AcceptThenDrop` scheduling.
    connected_tick: HashMap<OverlayAddress, usize>,
    /// Tick at which a `HandshakeFail` peer entered the handshake phase.
    handshaking_tick: HashMap<OverlayAddress, usize>,
    max_po: u8,
    saturation: usize,
    low_watermark: u8,
    /// Strictly increasing source of forged gossip timestamps.
    gossip_clock: i64,
    dials_per_tick: usize,
    rng: StdRng,
    tick_no: usize,
}

impl SimWorld {
    fn new(config: KademliaConfig, dials_per_tick: usize, seed: u64) -> Self {
        let base = OverlayAddress::from([0u8; 32]);
        let identity = MockIdentity::with_overlay(base);
        let low_watermark = identity.spec().neighborhood_low_watermark();
        let peer_manager = PeerManager::new(&identity, PeerManagerConfig::default());
        let routing = KademliaRouting::new(identity, config, peer_manager.clone());
        let max_po = routing.max_bin().get();
        let saturation = routing.limits().saturation();
        Self {
            base,
            routing,
            peer_manager,
            population: HashMap::new(),
            phases: HashMap::new(),
            connected_tick: HashMap::new(),
            handshaking_tick: HashMap::new(),
            max_po,
            saturation,
            low_watermark,
            gossip_clock: 0,
            dials_per_tick,
            rng: StdRng::seed_from_u64(seed),
            tick_no: 0,
        }
    }

    fn saturation(&self) -> usize {
        self.saturation
    }

    fn base(&self) -> OverlayAddress {
        self.base
    }

    fn depth(&self) -> NeighborhoodDepth {
        self.routing.depth()
    }

    /// The neighborhood-stability clock as the readiness snapshot reads it.
    fn stable_for(&self) -> Option<std::time::Duration> {
        self.routing.neighborhood_stable_for()
    }

    /// The [`Bin`] a peer occupies in this table (proximity to the local
    /// overlay, capped at `max_po`), mirroring the routing table's own mapping.
    fn bin_for(&self, overlay: &OverlayAddress) -> Bin {
        let po = self.base.proximity(overlay).get().min(self.max_po);
        Bin::new(po).unwrap_or(Bin::MAX)
    }

    /// Store a peer as known (gossip discovery) with a strictly newer timestamp.
    fn gossip(&mut self, overlay: OverlayAddress) {
        self.gossip_clock += 1;
        let peer = forge_peer(overlay, self.gossip_clock);
        self.peer_manager.store_discovered_peer(peer);
    }

    /// Register a single scripted peer and make it known to the peer manager.
    fn add_scripted(
        &mut self,
        overlay: OverlayAddress,
        node_type: SwarmNodeType,
        script: PeerScript,
    ) {
        self.population
            .insert(overlay, SimPeer { node_type, script });
        self.gossip(overlay);
    }

    /// Populate `bin` with `count` scripted peers at deterministic overlays.
    fn populate_bin(
        &mut self,
        bin: u8,
        count: usize,
        node_type: SwarmNodeType,
        script: PeerScript,
    ) {
        for idx in 0..count {
            let overlay = overlay_in_bin(self.base, bin, idx as u8);
            self.add_scripted(overlay, node_type, script);
        }
    }

    fn has_queued_candidates(&self) -> bool {
        self.routing.has_queued_candidates()
    }

    fn evaluate(&self) {
        self.routing.evaluate_connections();
    }

    fn pop_candidate(&self) -> Option<OverlayAddress> {
        self.routing.pop_candidate()
    }

    fn connected_in_bin(&self, bin: u8) -> usize {
        self.routing
            .bin_peer_counts(Bin::new(bin).unwrap_or(Bin::MAX))
            .0
    }

    /// Outbound (self-dialed) connections in `bin` from the routing table.
    fn outbound_in_bin(&self, bin: u8) -> usize {
        self.routing
            .bin_outbound_count(Bin::new(bin).unwrap_or(Bin::MAX))
    }

    /// Distinct sub-prefix slots among the connected peers in `bin`, via the
    /// production `slot_of` so the observation and the selection mechanism
    /// share one slot definition. One means a monoculture: every admitted peer
    /// shares a sub-trie.
    fn distinct_slots_in_bin(&self, bin: u8) -> usize {
        self.routing.filled_slots(Bin::new(bin).unwrap_or(Bin::MAX))
    }

    /// Advance one tick: evaluate, drain dials, apply scripted churn, refresh
    /// depth, then verify the invariants.
    fn tick(&mut self) {
        self.tick_no += 1;
        self.evaluate();
        self.drain_dials();
        self.apply_scripted_handshake_failures();
        self.apply_scripted_drops();
        self.routing.refresh_depth();
        self.assert_invariants();
    }

    /// Pop up to `dials_per_tick` candidates and resolve each against its
    /// script through the real capacity and peer-manager transitions.
    fn drain_dials(&mut self) {
        let mut dialed = 0;
        while dialed < self.dials_per_tick {
            let Some(overlay) = self.routing.pop_candidate() else {
                break;
            };
            dialed += 1;
            // A popped candidate is never one the routing/peer state should have
            // excluded: not connected, not banned, not in backoff.
            assert!(
                !matches!(self.phases.get(&overlay), Some(SimPhase::Active(_))),
                "popped a connected peer"
            );
            assert!(
                !self.peer_manager.is_banned(&overlay),
                "popped a banned peer"
            );
            assert!(
                !self.peer_manager.peer_is_in_backoff(&overlay),
                "popped a backoff peer"
            );
            self.resolve_dial(overlay);
        }
    }

    fn resolve_dial(&mut self, overlay: OverlayAddress) {
        let Some(sim) = self.population.get(&overlay).copied() else {
            return;
        };
        if !self.routing.try_reserve_dial(&overlay, sim.node_type) {
            // At capacity or already tracked; leave the peer for a later round.
            return;
        }
        self.phases.insert(overlay, SimPhase::Dialing);
        match sim.script {
            PeerScript::Unreachable => {
                self.routing.release_dial(&overlay);
                self.peer_manager.record_dial_failure(&overlay);
                self.phases.remove(&overlay);
            }
            PeerScript::HandshakeFail => {
                self.routing.dial_connected(&overlay);
                self.phases.insert(
                    overlay,
                    SimPhase::Handshaking(ConnectionDirection::Outbound),
                );
                self.handshaking_tick.insert(overlay, self.tick_no);
            }
            PeerScript::Honest | PeerScript::Sybil | PeerScript::AcceptThenDrop { .. } => {
                self.complete_outbound(overlay, sim.node_type);
            }
        }
    }

    /// The successful outbound path, collapsed to run synchronously: dial
    /// connected, handshake completed, peer-manager connect, routing connect.
    fn complete_outbound(&mut self, overlay: OverlayAddress, node_type: SwarmNodeType) {
        self.routing.dial_connected(&overlay);
        self.phases.insert(
            overlay,
            SimPhase::Handshaking(ConnectionDirection::Outbound),
        );
        self.routing.handshake_completed(&overlay);
        self.gossip_clock += 1;
        let peer = forge_peer(overlay, self.gossip_clock);
        self.peer_manager.on_peer_connected(
            peer,
            node_type,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        SwarmRouting::connected(&*self.routing, overlay);
        self.phases
            .insert(overlay, SimPhase::Active(ConnectionDirection::Outbound));
        self.connected_tick.insert(overlay, self.tick_no);
    }

    /// The inbound admission path: gate, reserve, complete, connect. Returns
    /// false when the bin's inbound ceiling refuses the connection.
    fn flood_inbound(&mut self, overlay: OverlayAddress, node_type: SwarmNodeType) -> bool {
        if !self.routing.should_accept_inbound(&overlay, node_type) {
            return false;
        }
        self.routing.reserve_inbound(&overlay);
        self.phases
            .insert(overlay, SimPhase::Handshaking(ConnectionDirection::Inbound));
        self.routing.handshake_completed(&overlay);
        self.gossip_clock += 1;
        let peer = forge_peer(overlay, self.gossip_clock);
        self.peer_manager.on_peer_connected(
            peer,
            node_type,
            ConnectionDirection::Inbound,
            TrustLevel::Normal,
        );
        SwarmRouting::connected(&*self.routing, overlay);
        self.phases
            .insert(overlay, SimPhase::Active(ConnectionDirection::Inbound));
        self.connected_tick.insert(overlay, self.tick_no);
        true
    }

    /// Clean disconnect (no dial-failure backoff), keeping the peer re-dialable.
    fn disconnect(&mut self, overlay: OverlayAddress) {
        RoutingCapacity::disconnected(&*self.routing, &overlay);
        SwarmRouting::on_peer_disconnected(&*self.routing, &overlay);
        self.peer_manager
            .on_peer_disconnected(&overlay, DisconnectReason::RemoteClose);
        self.phases.remove(&overlay);
        self.connected_tick.remove(&overlay);
    }

    /// Disconnect every active peer whose bin is deeper than `bin`, returning
    /// the severed overlays so the caller can rescript them.
    fn disconnect_bins_above(&mut self, bin: u8) -> Vec<OverlayAddress> {
        let victims: Vec<OverlayAddress> = self
            .phases
            .iter()
            .filter(|(overlay, phase)| {
                matches!(phase, SimPhase::Active(_)) && self.bin_for(overlay).get() > bin
            })
            .map(|(overlay, _)| *overlay)
            .collect();
        for overlay in &victims {
            self.disconnect(*overlay);
        }
        victims
    }

    /// Rescript an existing member of the population.
    fn set_script(&mut self, overlay: OverlayAddress, script: PeerScript) {
        if let Some(sim) = self.population.get_mut(&overlay) {
            sim.script = script;
        }
    }

    /// Disconnect a seeded `fraction` of the currently active peers.
    fn churn_burst(&mut self, fraction: f64) {
        let mut active: Vec<OverlayAddress> = self
            .phases
            .iter()
            .filter(|(_, phase)| matches!(phase, SimPhase::Active(_)))
            .map(|(overlay, _)| *overlay)
            .collect();
        // Sort before the seeded shuffle so the selection is reproducible
        // regardless of the hash-map iteration order.
        active.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        active.shuffle(&mut self.rng);
        let take = ((active.len() as f64) * fraction) as usize;
        for overlay in active.into_iter().take(take) {
            self.disconnect(overlay);
        }
    }

    /// Fail the handshake of any `HandshakeFail` peer that entered the phase
    /// on an earlier tick, mirroring the failure sequence the connection
    /// handlers run: release the handshake reservation, then arm the dial
    /// backoff. The one-tick hold means the previous tick's reconciliation
    /// observed the held handshake phase before this release drains it.
    fn apply_scripted_handshake_failures(&mut self) {
        let due: Vec<OverlayAddress> = self
            .phases
            .iter()
            .filter(|(overlay, phase)| {
                matches!(phase, SimPhase::Handshaking(_))
                    && matches!(
                        self.population.get(overlay).map(|sim| sim.script),
                        Some(PeerScript::HandshakeFail)
                    )
                    && self
                        .handshaking_tick
                        .get(overlay)
                        .is_some_and(|entered| *entered < self.tick_no)
            })
            .map(|(overlay, _)| *overlay)
            .collect();
        for overlay in due {
            self.routing.release_handshake(&overlay);
            self.peer_manager.record_dial_failure(&overlay);
            self.phases.remove(&overlay);
            self.handshaking_tick.remove(&overlay);
        }
    }

    /// Fire the scheduled clean drop for any `AcceptThenDrop` peer whose window
    /// has elapsed in ticks.
    fn apply_scripted_drops(&mut self) {
        let due: Vec<OverlayAddress> = self
            .population
            .iter()
            .filter_map(|(overlay, sim)| {
                let PeerScript::AcceptThenDrop { after_ticks } = sim.script else {
                    return None;
                };
                if !matches!(self.phases.get(overlay), Some(SimPhase::Active(_))) {
                    return None;
                }
                let since = self
                    .connected_tick
                    .get(overlay)
                    .copied()
                    .unwrap_or(self.tick_no);
                (self.tick_no.saturating_sub(since) >= after_ticks).then_some(*overlay)
            })
            .collect();
        for overlay in due {
            self.disconnect(overlay);
        }
    }

    /// Whether the shadow model holds `overlay` in the handshake phase.
    fn is_handshaking(&self, overlay: &OverlayAddress) -> bool {
        matches!(self.phases.get(overlay), Some(SimPhase::Handshaking(_)))
    }

    /// Whether the shadow model tracks `overlay` in any connection phase.
    fn is_tracked(&self, overlay: &OverlayAddress) -> bool {
        self.phases.contains_key(overlay)
    }

    /// Whether the peer manager holds `overlay` in dial backoff.
    fn in_backoff(&self, overlay: &OverlayAddress) -> bool {
        self.peer_manager.peer_is_in_backoff(overlay)
    }

    /// Instantaneous depth recomputed from the live connected-peer bin sizes,
    /// using the same nectar port and thresholds as the routing table.
    fn raw_depth(&self) -> NeighborhoodDepth {
        let sizes = self.routing.connected_peers.bin_sizes();
        let mut counts = [0u8; 32];
        for (slot, size) in counts.iter_mut().zip(sizes.iter()) {
            *slot = u8::try_from(*size).unwrap_or(u8::MAX);
        }
        let sat = u8::try_from(self.saturation).unwrap_or(u8::MAX);
        let bin = recompute_neighborhood_depth(&counts, sat, self.low_watermark);
        NeighborhoodDepth::new(Bin::new(bin.get().min(self.max_po)).unwrap_or(Bin::MAX))
    }

    /// Count of distinct connected peers held in the shadow phase model.
    fn active_count(&self) -> usize {
        self.phases
            .values()
            .filter(|phase| matches!(phase, SimPhase::Active(_)))
            .count()
    }

    /// The routing table's connected total equals the distinct active shadow
    /// count and the sum of its own per-bin sizes: no peer is double-counted.
    fn assert_no_double_count(&self) {
        let bin_sum: usize = self.routing.connected_peers.bin_sizes().iter().sum();
        assert_eq!(
            self.routing.connected_peers_total(),
            bin_sum,
            "connected total disagrees with the per-bin sizes"
        );
        assert_eq!(
            self.routing.connected_peers_total(),
            self.active_count(),
            "connected total disagrees with the active shadow"
        );
    }

    /// Every-tick invariants:
    /// - the table's per-bin phase counters match the shadow model;
    /// - the table's per-bin outbound counter matches the outbound shadow
    ///   (the counter is maintained on every lifecycle path, both directions);
    /// - every bin below the published depth has a target at or above
    ///   saturation (the allocation floor);
    /// - the published depth never sits below the instantaneous recompute
    ///   (the hysteresis holds lowers, never raises);
    /// - no peer is double-counted.
    fn assert_invariants(&self) {
        let depth = self.depth();

        // Per-bin (dialing, handshaking, active) and a separate outbound tally.
        let mut shadow: HashMap<Bin, (usize, usize, usize)> = HashMap::new();
        let mut outbound_shadow: HashMap<Bin, usize> = HashMap::new();
        for (overlay, phase) in &self.phases {
            let bin = self.bin_for(overlay);
            let entry = shadow.entry(bin).or_insert((0, 0, 0));
            match phase {
                SimPhase::Dialing => entry.0 += 1,
                SimPhase::Handshaking(_) => entry.1 += 1,
                SimPhase::Active(_) => entry.2 += 1,
            }
            if phase.is_outbound() {
                *outbound_shadow.entry(bin).or_insert(0) += 1;
            }
        }

        for bin in all_bins(self.routing.max_bin()) {
            let counts = self.routing.bin_phase_counts(bin);
            let shadowed = shadow.get(&bin).copied().unwrap_or((0, 0, 0));
            assert_eq!(
                counts,
                shadowed,
                "phase counters diverged at bin {}",
                bin.get()
            );

            assert_eq!(
                self.routing.bin_outbound_count(bin),
                outbound_shadow.get(&bin).copied().unwrap_or(0),
                "outbound counter diverged at bin {}",
                bin.get()
            );

            if !depth.contains(bin) {
                assert!(
                    self.routing.limits().target(bin, depth) >= self.saturation,
                    "below-depth bin {} target dropped under saturation",
                    bin.get()
                );
            }
        }

        let raw = self.raw_depth();
        assert!(
            depth.get() >= raw.get(),
            "published depth {} sits below the raw recompute {}",
            depth.get(),
            raw.get()
        );

        self.assert_no_double_count();
    }
}
