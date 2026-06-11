//! Protocol event handlers for topology behaviour.

use std::time::Duration;

use libp2p::PeerId;
use libp2p::ping;
use libp2p::swarm::{ConnectionId, ToSwarm};
use metrics::gauge;
use tracing::{debug, info, trace, warn};
use vertex_net_local::{AddressScope, classify_multiaddr};
use vertex_net_peer_registry::ActivateResult;
use vertex_swarm_api::{ReportSource, SwarmIdentity, SwarmScoringEvent};
use vertex_swarm_net_handshake::HandshakeEvent;
use vertex_swarm_net_hive::HiveEvent;
use vertex_swarm_peer_manager::TrustLevel;
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

use crate::DialReason;
use crate::composed::ProtocolEvent;
use crate::error::RejectionReason;
use crate::events::{ConnectionDirection, TopologyEvent};
use crate::gossip::GossipInput;
use crate::kademlia::{RoutingCapacity, SwarmRouting};

use crate::behaviour::TopologyBehaviour;

impl<I: SwarmIdentity + Clone> TopologyBehaviour<I> {
    #[tracing::instrument(skip_all, level = "trace", fields(%peer_id))]
    pub(crate) fn process_protocol_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: ProtocolEvent,
    ) {
        match event {
            ProtocolEvent::Handshake(HandshakeEvent::Completed { info, .. }) => {
                self.on_handshake_completed(peer_id, connection_id, *info);
            }
            ProtocolEvent::Handshake(HandshakeEvent::Failed { error, .. }) => {
                self.on_handshake_failed(peer_id, error);
            }
            ProtocolEvent::Hive(HiveEvent::PeersReceived { peers, .. }) => {
                self.on_hive_peers_received(peer_id, peers);
            }
            ProtocolEvent::Hive(HiveEvent::Error { error, .. }) => {
                warn!(%peer_id, %error, "Hive error");
            }
            ProtocolEvent::Ping(ping::Event { result, .. }) => {
                self.on_ping_result(peer_id, result);
            }
        }
    }

    #[tracing::instrument(skip(self, info), level = "debug", fields(%peer_id))]
    fn on_handshake_completed(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        info: vertex_swarm_net_handshake::HandshakeInfo,
    ) {
        let overlay = OverlayAddress::from(*info.swarm_peer.overlay());
        let node_type = info.node_type;

        debug!(
            %peer_id,
            %overlay,
            ?node_type,
            bin = self.bin_for(&overlay).get(),
            "Handshake completed"
        );

        // Get dial info from connection registry before transitioning
        let current_state = self.connection_registry.get(&overlay).or_else(|| {
            self.connection_registry
                .resolve_id(&peer_id)
                .and_then(|o| self.connection_registry.get(&o))
        });
        let direction = current_state
            .as_ref()
            .and_then(|s| s.direction())
            .unwrap_or(ConnectionDirection::Inbound);
        let dial_reason = current_state.as_ref().and_then(|s| *s.reason());

        // Reject banned peers immediately (inbound peers bypass dial-time ban check).
        if self.peer_manager.is_banned(&overlay) {
            debug!(
                %peer_id,
                %overlay,
                ?direction,
                "Rejecting connection: peer is banned"
            );
            self.emit_event(TopologyEvent::PeerRejected {
                overlay,
                peer_id,
                reason: RejectionReason::Banned,
                direction,
            });
            self.pending_actions.push_back(ToSwarm::CloseConnection {
                peer_id,
                connection: libp2p::swarm::CloseConnection::All,
            });
            return;
        }

        // An outbound dial was guided by a stored record; if the handshake
        // asserts a different overlay, that record's address belongs to
        // another peer. The peer that answered proceeds normally (and is
        // stored and verified below); the record that pointed here is
        // demoted: removed if it was an unverified gossip claim, or given a
        // dial failure if it was verified once. The dialed overlay's
        // handshake reservation is released since the completion below is
        // keyed by the asserted overlay. This runs after the ban check: on
        // the banned early return the close handler releases the dialed
        // overlay's reservation itself, and reserving a slot for the
        // asserted overlay there would leak it.
        if let Some(dialed_overlay) = current_state.as_ref().and_then(|s| s.id())
            && dialed_overlay != overlay
        {
            warn!(
                %peer_id,
                dialed = %dialed_overlay,
                asserted = %overlay,
                "handshake asserted a different overlay than the dialed record"
            );
            self.routing.release_handshake(&dialed_overlay);
            self.peer_manager
                .on_dialed_overlay_mismatch(&dialed_overlay);
            // The asserted overlay holds no reservation of its own; account
            // for it like an unsolicited inbound peer so the capacity
            // counters stay symmetric with the disconnect path.
            RoutingCapacity::reserve_inbound(&*self.routing, &overlay);
        }

        // For inbound connections, check bin capacity and reserve a slot before
        // transitioning to active. Outbound connections already reserved capacity
        // at dial time via try_reserve_dial.
        if direction == ConnectionDirection::Inbound {
            let bin_at_capacity =
                !RoutingCapacity::should_accept_inbound(&*self.routing, &overlay, node_type);
            if bin_at_capacity {
                debug!(
                    %peer_id,
                    %overlay,
                    ?node_type,
                    ?direction,
                    "Rejecting inbound connection: bin saturated"
                );
                self.emit_event(TopologyEvent::PeerRejected {
                    overlay,
                    peer_id,
                    reason: RejectionReason::BinSaturated,
                    direction,
                });
                self.pending_actions.push_back(ToSwarm::CloseConnection {
                    peer_id,
                    connection: libp2p::swarm::CloseConnection::All,
                });
                return;
            }
            // Reserve inbound slot so handshake_completed can transition Handshaking->Active
            RoutingCapacity::reserve_inbound(&*self.routing, &overlay);
        }

        // Transition to Active state in connection registry
        let activate_result = self
            .connection_registry
            .activate(peer_id, connection_id, overlay);
        match &activate_result {
            ActivateResult::Accepted => {
                gauge!("peer_registry_pending_connections").decrement(1.0);
                gauge!("peer_registry_active_connections").increment(1.0);
            }
            ActivateResult::Replaced { old_id: None, .. } => {
                gauge!("peer_registry_pending_connections").decrement(1.0);
            }
            ActivateResult::Replaced {
                old_id: Some(_), ..
            } => {}
        }

        // Update routing capacity tracking (transitions Handshaking->Active)
        RoutingCapacity::handshake_completed(&*self.routing, &overlay);

        // Handle the activate result from connection registry
        match activate_result {
            ActivateResult::Replaced {
                old_peer_id,
                old_connection_id,
                ref old_id,
            } => {
                // The old connection was already counted by a prior PeerReady event.
                // Its registry entry is now overwritten, so handle_connection_closed
                // will not emit PeerDisconnected -- we must decrement here.
                // The peer manager holds the authoritative handshake-confirmed
                // node type; on_peer_connected for the new connection has not
                // run yet, so this still reads the old connection's value.
                let old_overlay = old_id.as_ref().unwrap_or(&overlay);
                let old_node_type = self
                    .peer_manager
                    .node_type(old_overlay)
                    .unwrap_or(SwarmNodeType::Client);
                self.metrics.decrement_connected(old_node_type);
                gauge!("peer_registry_active_connections").decrement(1.0);

                debug!(
                    %peer_id,
                    %old_peer_id,
                    ?old_connection_id,
                    %overlay,
                    "Closing old connection, new connection takes over"
                );
                self.emit_event(TopologyEvent::PeerRejected {
                    overlay,
                    peer_id: old_peer_id,
                    reason: RejectionReason::DuplicateConnection,
                    direction,
                });
                // Close only the specific old connection, not all connections.
                // This handles racing dialers (same PeerId claiming same overlay)
                // correctly by keeping the new connection active.
                self.pending_actions.push_back(ToSwarm::CloseConnection {
                    peer_id: old_peer_id,
                    connection: libp2p::swarm::CloseConnection::One(old_connection_id),
                });
            }
            ActivateResult::Accepted => {}
        }

        // Store peer metadata and connection state. The trust level is
        // computed here because topology owns the dial reason (explicitly
        // configured peers dial with `DialReason::Trusted`) and the listen
        // addresses needed to judge subnet locality; the peer manager stores
        // the result so eviction ranking reads one atomic instead of
        // re-deriving address scope per trim round.
        let trust = if dial_reason == Some(DialReason::Trusted) {
            TrustLevel::Trusted
        } else if crate::behaviour::peer_is_local(
            &info.swarm_peer,
            &self.nat_discovery.listen_addrs(),
        ) {
            TrustLevel::LocalSubnet
        } else {
            TrustLevel::Normal
        };
        let remote_ip = self.connection_remote_ips.get(&connection_id).copied();
        self.peer_manager.on_peer_connected(
            info.swarm_peer.clone(),
            info.node_type,
            direction,
            trust,
            remote_ip,
        );

        // Feed reachability BEFORE notifying routing: `trim_overpopulated_bins`
        // ranks eviction victims by reachability (least-reachable first), so the
        // freshly-connected peer's status must be settled before
        // `routing.connected` and any resulting trim reads its rank.
        //
        // A completed handshake is only a liveness signal. We promote to Public
        // solely when *we* dialed the peer at a public-scope address (recorded
        // at connection establishment): reaching a public address proves it is
        // dialable. An inbound handshake (the peer dialed us over an ephemeral
        // port) proves nothing about the peer's inbound reachability, so it
        // stays liveness-only; AutoNAT v2 dial-back is the other promotion path.
        let reachability = self.nat_discovery.reachability();
        if self.outbound_public_dials.remove(&connection_id) {
            reachability.on_outbound_reachable(peer_id);
        } else {
            reachability.update_from_handshake(peer_id, true);
        }

        // An inbound connection means the peer dialed us and reached us at the
        // address it reports observing. If that address is public it is our
        // genuinely reachable listen address (not an ephemeral outbound port),
        // so record it as a weak public-connectivity signal for ourselves.
        if direction == ConnectionDirection::Inbound
            && !info.observed_multiaddr.is_empty()
            && classify_multiaddr(&info.observed_multiaddr) == Some(AddressScope::Public)
        {
            self.nat_discovery
                .on_observed_addr(&info.observed_multiaddr);
        }

        let bin = self.bin_for(&overlay);

        let old_depth = self.routing.depth();
        self.routing.connected(overlay);
        let new_depth = self.routing.depth();

        // Push event-driven routing gauges for the affected bin
        self.push_routing_gauges(bin);

        if new_depth != old_depth {
            self.on_depth_changed(old_depth, new_depth);
        }

        self.refresh_topology_phase();

        self.emit_event(TopologyEvent::PeerReady {
            overlay,
            peer_id,
            node_type,
            direction,
        });

        // Notify gossip task -- exchange happens immediately or after delay (for gossip dials)
        self.gossip.send(GossipInput::PeerActivated {
            peer_id,
            swarm_peer: info.swarm_peer,
            node_type,
        });

        // Dial completed successfully - coalesced evaluation in poll()
        self.evaluator_handle.trigger_evaluation();
    }

    fn on_handshake_failed(
        &mut self,
        peer_id: PeerId,
        error: vertex_swarm_net_handshake::HandshakeError,
    ) {
        warn!(%peer_id, %error, "Handshake failed");

        // Only feed the reachability tracker on errors that are unambiguously
        // the peer's fault. Timeouts, connection-closed-by-either-side, IO,
        // and bare upgrade errors can be triggered by our own actions
        // (duplicate-connection eviction, ban-by-remote, shutdown) and would
        // unfairly demote innocent peers.
        if is_peer_fault(&error) {
            self.nat_discovery
                .reachability()
                .update_from_handshake(peer_id, false);
        }

        // Handshake failed means the peer was already registered in connection_registry.
        // Remove it and release routing capacity.
        let state = self.connection_registry.disconnected(&peer_id);
        if let Some(ref s) = state {
            if s.is_active() {
                gauge!("peer_registry_active_connections").decrement(1.0);
            } else if s.is_pending() {
                gauge!("peer_registry_pending_connections").decrement(1.0);
            }
        }
        let reason = state.as_ref().and_then(|s| *s.reason());
        let overlay = state.as_ref().and_then(|s| s.id());

        if let Some(ref overlay) = overlay {
            self.routing.release_handshake(overlay);
            self.peer_manager.record_dial_failure(overlay);
            // Score the failure only when the peer is unambiguously at
            // fault, mirroring the reachability gate above; our own
            // duplicate-connection evictions and shutdowns must not
            // penalize innocent peers.
            if is_peer_fault(&error) {
                self.peer_manager.report_peer(
                    overlay,
                    SwarmScoringEvent::HandshakeFailure,
                    ReportSource::Handshake,
                );
            }
        }

        self.emit_event(TopologyEvent::DialFailed {
            overlay,
            addrs: Vec::new(),
            error: crate::error::DialError::HandshakeFailed(error.to_string()),
            dial_duration: state
                .as_ref()
                .and_then(|s| s.started_at())
                .map(|t| t.elapsed()),
            reason,
        });
    }

    fn on_hive_peers_received(
        &mut self,
        peer_id: PeerId,
        peers: Vec<vertex_swarm_peer::SwarmPeer>,
    ) {
        if peers.is_empty() {
            return;
        }

        // Filter peers we can't reach based on our IP capability.
        let local_capability = self.nat_discovery.capability();
        let peers: Vec<vertex_swarm_peer::SwarmPeer> = if local_capability.is_known() {
            peers
                .into_iter()
                .filter(|peer| {
                    let peer_cap = peer.ip_capability();
                    let reachable = local_capability.can_reach(&peer_cap);
                    if !reachable {
                        trace!(
                            overlay = %peer.overlay(),
                            ?local_capability,
                            ?peer_cap,
                            "filtering unreachable gossiped peer"
                        );
                    }
                    reachable
                })
                .collect()
        } else {
            // Capability unknown (no listen addrs yet) -- let all through
            peers
        };

        if peers.is_empty() {
            return;
        }

        let gossiper = self
            .connection_registry
            .resolve_id(&peer_id)
            .unwrap_or_else(|| {
                warn!(%peer_id, "Hive peers from unknown peer");
                OverlayAddress::default()
            });

        let peer_count = peers.len();
        self.gossip
            .send(GossipInput::PeersReceived { gossiper, peers });

        // Disconnect from bootnodes after receiving the initial peer list.
        // Bootnodes are gossip amplifiers -- every new peer connecting to the bootnode
        // triggers a hive stream to all existing connections. Staying connected produces
        // a flood of 1-peer hive messages (~2/s on mainnet) that overwhelms rate limiters.
        let reason = self
            .connection_registry
            .get(&gossiper)
            .and_then(|s| *s.reason());
        if reason == Some(DialReason::Bootnode) {
            info!(
                %peer_id,
                %gossiper,
                peer_count,
                "Disconnecting from bootnode after initial hive gossip"
            );
            self.pending_actions.push_back(ToSwarm::CloseConnection {
                peer_id,
                connection: libp2p::swarm::CloseConnection::All,
            });
        }
    }

    /// Handle a `libp2p::ping` round-trip result.
    ///
    /// Success records RTT (latency + score) and is a positive liveness signal
    /// for the reachability tracker; a failed/timed-out ping is a negative one
    /// (the tracker demotes after a streak). This is the same mechanism the
    /// reference implementation's reacher uses to judge peer reachability.
    fn on_ping_result(&mut self, peer_id: PeerId, result: Result<Duration, ping::Failure>) {
        let tracker = self.nat_discovery.reachability();
        match result {
            Ok(rtt) => {
                tracker.update_from_ping(peer_id, true);
                if let Some(overlay) = self.connection_registry.resolve_id(&peer_id) {
                    self.peer_manager.record_latency(&overlay, rtt);
                    debug!(%peer_id, %overlay, ?rtt, "ping ok: liveness + rtt");
                    self.emit_event(TopologyEvent::PingCompleted { overlay, rtt });
                }
            }
            Err(failure) => {
                tracker.update_from_ping(peer_id, false);
                debug!(%peer_id, %failure, "ping failed");
            }
        }
    }
}

/// Classify a handshake error: only protocol violations the peer is solely
/// responsible for should feed the reachability tracker. Timeouts, IO
/// errors, and bare connection-close events can be caused by our own side
/// (duplicate-connection eviction, shutdown, ban-by-remote) and would
/// otherwise demote innocent peers.
fn is_peer_fault(error: &vertex_swarm_net_handshake::HandshakeError) -> bool {
    use vertex_swarm_net_handshake::HandshakeError as E;
    matches!(
        error,
        E::NetworkIdMismatch
            | E::MissingField(_)
            | E::FieldTooLong { .. }
            | E::InvalidData(_)
            | E::InvalidMultiaddr(_)
            | E::InvalidSignature(_)
            | E::InvalidPeer(_)
            | E::InvalidOverlay
            | E::InvalidObservedAddress
            | E::Protobuf(_)
    )
}
