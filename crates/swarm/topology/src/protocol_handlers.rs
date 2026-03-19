//! Protocol event handlers for topology behaviour.

use std::time::Duration;

use libp2p::PeerId;
use libp2p::swarm::{ConnectionId, ToSwarm};
use metrics::gauge;
use tracing::{debug, info, trace, warn};
use vertex_net_peer_registry::ActivateResult;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_net_handshake::HandshakeEvent;
use vertex_swarm_net_hive::HiveEvent;
use vertex_swarm_net_pingpong::PingpongEvent;
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
            ProtocolEvent::Pingpong(PingpongEvent::Pong { rtt, .. }) => {
                self.on_pingpong_pong(peer_id, rtt);
            }
            ProtocolEvent::Pingpong(PingpongEvent::PingReceived { .. }) => {
                debug!(%peer_id, "Received ping from peer");
            }
            ProtocolEvent::Pingpong(PingpongEvent::Error { error, .. }) => {
                warn!(%peer_id, %error, "Pingpong failed");
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
            po = self.proximity(&overlay),
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
                // Use connected_node_types (recorded at PeerReady time) for symmetric decrement.
                let old_overlay = old_id.as_ref().unwrap_or(&overlay);
                let old_node_type = self
                    .connected_node_types
                    .remove(old_overlay)
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

        // Store peer metadata
        self.peer_manager
            .on_peer_ready(info.swarm_peer.clone(), info.node_type);

        let po = self.proximity(&overlay);

        let old_depth = self.routing.depth();
        self.routing.connected(overlay);
        let new_depth = self.routing.depth();

        // Push event-driven routing gauges for the affected bin
        self.push_routing_gauges(po);

        if new_depth != old_depth {
            self.push_bin_targets();
            self.gossip.send(GossipInput::DepthChanged(new_depth));
            self.emit_event(TopologyEvent::DepthChanged {
                old_depth,
                new_depth,
            });
            if new_depth > old_depth {
                self.trim_overpopulated_bins();
            }
        }

        // Record node_type for symmetric decrement on disconnect.
        self.connected_node_types.insert(overlay, node_type);

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

    fn on_pingpong_pong(&mut self, peer_id: PeerId, rtt: Duration) {
        debug!(%peer_id, ?rtt, "Pingpong success");

        if let Some(overlay) = self.connection_registry.resolve_id(&peer_id) {
            self.peer_manager.record_latency(&overlay, rtt);
            debug!(%peer_id, %overlay, ?rtt, "Connection health verified");

            self.emit_event(TopologyEvent::PingCompleted { overlay, rtt });
        }
    }
}
