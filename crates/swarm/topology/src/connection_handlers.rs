//! Connection lifecycle handlers for topology behaviour.

use libp2p::swarm::ConnectionError;
use metrics::gauge;
use tracing::{debug, trace, warn};
use vertex_net_local::{AddressScope, classify_multiaddr};
use vertex_net_peer_registry::ConnectionState;
use vertex_swarm_api::{ReportSource, SwarmIdentity, SwarmScoringEvent};
use vertex_swarm_net_handshake::HANDSHAKE_TIMEOUT;
use vertex_swarm_primitives::SwarmNodeType;

use crate::error::{DialError, DisconnectReason};
use crate::events::TopologyEvent;
use crate::gossip::GossipInput;
use crate::kademlia::{RoutingCapacity, SwarmRouting};

use crate::behaviour::TopologyBehaviour;

/// Decrement the appropriate connection phase gauge based on the removed state.
fn decrement_connection_phase_gauge<Id: Clone, R>(state: &ConnectionState<Id, R>) {
    if state.is_active() {
        gauge!("peer_registry_active_connections").decrement(1.0);
    } else if state.is_pending() {
        gauge!("peer_registry_pending_connections").decrement(1.0);
    }
}

impl<I: SwarmIdentity + Clone> TopologyBehaviour<I> {
    pub(crate) fn handle_connection_established(
        &mut self,
        established: libp2p::swarm::behaviour::ConnectionEstablished,
    ) {
        if established.endpoint.is_dialer() {
            // Record outbound dials to a public-scope address. A successful
            // outbound connection proves the dialed address is reachable, so on
            // handshake completion this promotes the peer to Public. Private/LAN
            // successes prove only local reachability and are not recorded.
            if classify_multiaddr(established.endpoint.get_remote_address())
                == Some(AddressScope::Public)
            {
                self.outbound_public_dials.insert(established.connection_id);
            }

            // Resolve from DialTracker (sole source of outbound dial tracking)
            if let Some(request) = self.dial_tracker.resolve(&established.peer_id) {
                let overlay = request.id;
                let reason = request.data;
                let result = self.connection_registry.connected_outbound(
                    established.peer_id,
                    established.connection_id,
                    overlay,
                    request.queued_at(),
                    Some(reason),
                );
                if result.is_some() {
                    gauge!("peer_registry_pending_connections").increment(1.0);
                }
                if let Some(overlay) = &overlay {
                    self.routing.dial_connected(overlay);
                }
            } else {
                trace!(peer_id = %established.peer_id, "ConnectionEstablished for untracked outbound peer");
            }
        } else {
            self.connection_registry
                .connected_inbound(established.peer_id, established.connection_id);
            gauge!("peer_registry_pending_connections").increment(1.0);
        }
    }

    pub(crate) fn handle_connection_closed(
        &mut self,
        closed: libp2p::swarm::behaviour::ConnectionClosed,
    ) {
        // Drop any outbound-public marker for this specific connection,
        // regardless of whether other connections to the peer remain.
        self.outbound_public_dials.remove(&closed.connection_id);

        if closed.remaining_established > 0 {
            return;
        }

        // Drop the reachability record so memory does not accumulate for
        // transient or scanner peers. A subsequent reconnect rebuilds the
        // record from a clean slate, which is the correct behaviour for
        // peers we have no recent evidence about.
        self.nat_discovery.reachability().forget(&closed.peer_id);

        // Remove from connection registry (sole source of truth for connections)
        let removed_state = self.connection_registry.disconnected(&closed.peer_id);
        if let Some(ref s) = removed_state {
            decrement_connection_phase_gauge(s);
        }
        let connected_at = removed_state.as_ref().and_then(|s| s.connected_at());
        let overlay = removed_state.as_ref().and_then(|s| s.id());

        self.gossip.send(GossipInput::ConnectionClosed {
            peer_id: closed.peer_id,
            overlay,
        });

        let Some(overlay) = overlay else {
            // Unknown overlay connection closed — no routing capacity to release and
            // no routing table entry to update, so skip evaluation.
            self.metrics.record_unknown_overlay_disconnect();
            return;
        };

        // Read the authoritative node type from the peer manager. It is
        // handshake-confirmed there, so gossip received during the connection
        // cannot have changed it and the metric decrement stays symmetric
        // with the increment recorded at PeerReady time.
        let node_type = self
            .peer_manager
            .node_type(&overlay)
            .unwrap_or(SwarmNodeType::Client);

        let connection_duration = connected_at.map(|t| t.elapsed());
        debug!(
            peer_id = %closed.peer_id,
            %overlay,
            ?node_type,
            ?connection_duration,
            cause = ?closed.cause,
            "Peer disconnected"
        );

        // Release capacity slot
        RoutingCapacity::disconnected(&*self.routing, &overlay);

        // Push event-driven routing gauges for the affected bin
        let bin = self.bin_for(&overlay);
        self.push_routing_gauges(bin);

        // Capacity freed - coalesced evaluation in poll()
        self.evaluator_handle.trigger_evaluation();

        // Update routing tables
        let old_depth = self.routing.depth();
        SwarmRouting::on_peer_disconnected(&*self.routing, &overlay);
        let new_depth = self.routing.depth();

        // Determine disconnect reason from pending evictions and libp2p cause.
        let disconnect_reason = if self.pending_evictions.remove(&overlay) {
            DisconnectReason::BinTrimmed
        } else {
            match closed.cause {
                Some(ConnectionError::IO(_)) => DisconnectReason::ConnectionError,
                Some(ConnectionError::KeepAliveTimeout) => DisconnectReason::ConnectionError,
                // No error: orderly close initiated by local or remote side.
                None => DisconnectReason::LocalClose,
            }
        };

        // Penalize early disconnects (post-handshake connections that fail quickly).
        // Skip BinTrimmed since we initiated the eviction.
        if disconnect_reason != DisconnectReason::BinTrimmed
            && let Some(duration) = connection_duration
            && duration < self.early_disconnect_threshold
        {
            debug!(
                %overlay,
                ?duration,
                ?disconnect_reason,
                "early disconnect detected, applying penalty"
            );
            self.peer_manager
                .record_early_disconnect(&overlay, duration);
            self.metrics.record_early_disconnect(disconnect_reason);
        }

        // Clear the connection state on the peer record and emit the
        // Disconnected lifecycle event for subscribers.
        self.peer_manager
            .on_peer_disconnected(&overlay, disconnect_reason.into());

        self.emit_event(TopologyEvent::PeerDisconnected {
            overlay,
            reason: disconnect_reason,
            connection_duration,
            node_type,
        });

        if new_depth != old_depth {
            self.push_bin_targets();
            self.gossip.send(GossipInput::DepthChanged(new_depth.get()));
            self.emit_event(TopologyEvent::DepthChanged {
                old_depth: old_depth.get(),
                new_depth: new_depth.get(),
            });
            if new_depth > old_depth {
                self.trim_overpopulated_bins();
            }
        }
    }

    pub(crate) fn handle_dial_failure(&mut self, failure: libp2p::swarm::behaviour::DialFailure) {
        let Some(peer_id) = failure.peer_id else {
            trace!("DialFailure without peer_id");
            return;
        };

        // Resolve from DialTracker (sole source of outbound dial tracking)
        let Some(request) = self.dial_tracker.resolve(&peer_id) else {
            trace!(%peer_id, "DialFailure for unknown/untracked peer_id");
            return;
        };

        let overlay = request.id;
        let dial_duration = Some(request.queued_at().elapsed());

        let classified_error = classify_dial_error(failure.error);

        // Release routing capacity for this failed dial
        if let Some(overlay) = &overlay {
            self.routing.release_dial(overlay);
            self.peer_manager.record_dial_failure(overlay);

            // Score penalty based on error type, through the single report path
            let scoring_event = match &classified_error {
                DialError::ConnectionRefused => SwarmScoringEvent::ConnectionRefused,
                _ => SwarmScoringEvent::ConnectionTimeout,
            };
            self.peer_manager
                .report_peer(overlay, scoring_event, ReportSource::Topology);
        }

        warn!(
            %peer_id,
            ?overlay,
            ?classified_error,
            addr_count = request.addrs.len(),
            "Dial failed (all addresses exhausted)"
        );

        self.emit_event(TopologyEvent::DialFailed {
            overlay,
            addrs: request.addrs,
            error: classified_error,
            dial_duration,
            reason: Some(request.data),
        });
    }

    /// Clean up pending connections that have been waiting longer than HANDSHAKE_TIMEOUT.
    pub(crate) fn cleanup_stale_pending(&mut self) {
        // Clean up stale dials from the DialTracker (covers all outbound dials)
        let cleanup = self.dial_tracker.cleanup_expired();
        for request in cleanup.timed_out_in_flight {
            if let Some(overlay) = &request.id {
                self.routing.release_dial(overlay);
                self.peer_manager.record_dial_failure(overlay);
            }
            warn!(
                peer_id = %request.peer_id,
                overlay = ?request.id,
                timeout = ?HANDSHAKE_TIMEOUT,
                "Cleaning up stale dial from tracker"
            );
            let dial_duration = request.queued_at().elapsed();
            self.emit_event(TopologyEvent::DialFailed {
                overlay: request.id,
                addrs: request.addrs,
                error: DialError::Stale,
                dial_duration: Some(dial_duration),
                reason: Some(request.data),
            });
        }

        // Clean up stale handshakes from the connection registry
        // (connections that established TCP but handshake hasn't completed)
        let stale_peers = self.connection_registry.stale_pending(HANDSHAKE_TIMEOUT);

        for peer_id in stale_peers {
            if let Some(state) = self.connection_registry.disconnected(&peer_id) {
                decrement_connection_phase_gauge(&state);

                let reason = *state.reason();
                let overlay = state.id();

                if let Some(overlay) = &overlay {
                    self.routing.release_handshake(overlay);
                    self.peer_manager.record_dial_failure(overlay);
                }

                warn!(
                    %peer_id,
                    ?overlay,
                    timeout = ?HANDSHAKE_TIMEOUT,
                    "Cleaning up stale handshake"
                );

                self.emit_event(TopologyEvent::DialFailed {
                    overlay,
                    addrs: Vec::new(),
                    error: DialError::Stale,
                    dial_duration: state.started_at().map(|t| t.elapsed()),
                    reason,
                });
            }
        }
    }
}

/// Classify a libp2p dial error into a structured `DialError` variant.
pub(crate) fn classify_dial_error(error: &libp2p::swarm::DialError) -> DialError {
    use libp2p::core::transport::TransportError;
    use std::io::ErrorKind;

    match error {
        libp2p::swarm::DialError::Transport(addrs) => {
            // Classify based on the most informative transport error.
            // If all addresses failed with the same kind, use that; otherwise fall back.
            for (_, err) in addrs {
                match err {
                    TransportError::Other(io_err) => match io_err.kind() {
                        ErrorKind::TimedOut => return DialError::Timeout,
                        ErrorKind::ConnectionRefused => return DialError::ConnectionRefused,
                        ErrorKind::AddrNotAvailable
                        | ErrorKind::NetworkUnreachable
                        | ErrorKind::HostUnreachable => return DialError::Unreachable,
                        _ => {
                            // Check inner error message for nested timeout/refused
                            let msg = io_err.to_string().to_lowercase();
                            if msg.contains("timed out") || msg.contains("timeout") {
                                return DialError::Timeout;
                            }
                            if msg.contains("connection refused") {
                                return DialError::ConnectionRefused;
                            }
                            if msg.contains("no route") {
                                return DialError::NoRoute;
                            }
                            if msg.contains("unreachable") {
                                return DialError::Unreachable;
                            }
                            if msg.contains("negotiation") || msg.contains("multistream") {
                                return DialError::NegotiationFailed;
                            }
                        }
                    },
                    TransportError::MultiaddrNotSupported(_) => {}
                }
            }
            DialError::Other(format!("{error:?}"))
        }
        libp2p::swarm::DialError::Aborted | libp2p::swarm::DialError::DialPeerConditionFalse(_) => {
            DialError::Stale
        }
        libp2p::swarm::DialError::Denied { .. } => DialError::NegotiationFailed,
        libp2p::swarm::DialError::NoAddresses => DialError::NoRoute,
        libp2p::swarm::DialError::LocalPeerId { .. }
        | libp2p::swarm::DialError::WrongPeerId { .. } => DialError::Other(format!("{error:?}")),
    }
}
