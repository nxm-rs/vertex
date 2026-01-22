//! NetworkBehaviour implementation for the pricing protocol.
//!
//! This behaviour wraps the pricing handler and integrates it with the libp2p swarm.
//! Pricing exchange is initiated after handshake completion via `start_pricing()`.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    task::{Context, Poll},
};

use alloy_primitives::U256;
use libp2p::{
    core::{transport::PortUse, Endpoint},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler,
        THandlerInEvent, ToSwarm,
    },
    Multiaddr, PeerId,
};

use crate::{
    handler::{Command, Config, Event as HandlerEvent, Handler},
    PaymentThresholdObserver,
};

/// Events emitted by the pricing behaviour.
#[derive(Debug)]
pub enum PricingEvent {
    /// Successfully received a payment threshold from a peer.
    ThresholdReceived {
        /// The peer that announced the threshold.
        peer_id: PeerId,
        /// The connection on which the threshold was received.
        connection_id: ConnectionId,
        /// The payment threshold announced by the peer.
        threshold: U256,
    },
    /// Successfully sent our payment threshold to a peer.
    ThresholdSent {
        /// The peer we sent the threshold to.
        peer_id: PeerId,
    },
    /// A peer's threshold was below our minimum requirement.
    ThresholdTooLow {
        /// The peer that announced the low threshold.
        peer_id: PeerId,
        /// The threshold they announced.
        threshold: U256,
        /// Our minimum requirement.
        minimum: U256,
    },
    /// Protocol exchange failed.
    Error {
        /// The peer involved in the failure.
        peer_id: PeerId,
        /// Error description.
        error: String,
    },
}

/// Configuration for the pricing behaviour.
#[derive(Debug, Clone)]
pub struct PricingConfig {
    /// Handler configuration.
    pub handler_config: Config,
}

impl Default for PricingConfig {
    fn default() -> Self {
        Self {
            handler_config: Config::default(),
        }
    }
}

impl PricingConfig {
    /// Create a new pricing config for a full node.
    pub fn full_node() -> Self {
        Self {
            handler_config: Config {
                is_full_node: true,
                ..Default::default()
            },
        }
    }

    /// Create a new pricing config for a light node.
    pub fn light_node() -> Self {
        Self {
            handler_config: Config {
                is_full_node: false,
                ..Default::default()
            },
        }
    }
}

/// NetworkBehaviour for the pricing protocol.
///
/// This behaviour exchanges payment thresholds with connected peers.
/// Pricing exchange is NOT automatic - it must be triggered by calling
/// `start_pricing()` after the handshake completes.
///
/// When a peer's threshold is received, observers are notified so the accounting
/// system can track per-peer thresholds.
pub struct PricingBehaviour {
    /// Configuration for creating handlers.
    config: PricingConfig,
    /// Pending events to emit.
    events: VecDeque<ToSwarm<PricingEvent, Command>>,
    /// Observers to notify of threshold announcements.
    observers: Vec<Arc<dyn PaymentThresholdObserver>>,
    /// Map of peer ID to overlay address for notifying observers.
    /// This is populated by other parts of the system (e.g., after handshake).
    peer_overlays: HashMap<PeerId, [u8; 32]>,
    /// Map of peer ID to their connection IDs.
    peer_connections: HashMap<PeerId, Vec<ConnectionId>>,
}

impl PricingBehaviour {
    /// Create a new pricing behaviour with the given configuration.
    pub fn new(config: PricingConfig) -> Self {
        Self {
            config,
            events: VecDeque::new(),
            observers: Vec::new(),
            peer_overlays: HashMap::new(),
            peer_connections: HashMap::new(),
        }
    }

    /// Add an observer to be notified of payment threshold announcements.
    pub fn add_observer(&mut self, observer: Arc<dyn PaymentThresholdObserver>) {
        self.observers.push(observer);
    }

    /// Start the pricing exchange with a peer after handshake completion.
    ///
    /// This should be called when the handshake completes. It sends a command
    /// to all handlers for this peer to initiate the pricing exchange.
    ///
    /// # Arguments
    /// * `peer_id` - The peer to start pricing with
    /// * `overlay` - The peer's overlay address (from handshake)
    /// * `peer_is_full_node` - Whether the peer is a full node (from handshake ack)
    pub fn start_pricing(&mut self, peer_id: PeerId, overlay: [u8; 32], peer_is_full_node: bool) {
        // Register the overlay for observer notifications
        self.peer_overlays.insert(peer_id, overlay);

        // Send command to all handlers for this peer
        if let Some(connections) = self.peer_connections.get(&peer_id) {
            for &connection_id in connections {
                self.events.push_back(ToSwarm::NotifyHandler {
                    peer_id,
                    handler: NotifyHandler::One(connection_id),
                    event: Command::StartPricing { peer_is_full_node },
                });
            }
        }
    }

    /// Register a peer's overlay address without starting pricing.
    ///
    /// Use `start_pricing()` instead if you want to also trigger the exchange.
    pub fn register_peer_overlay(&mut self, peer_id: PeerId, overlay: [u8; 32]) {
        self.peer_overlays.insert(peer_id, overlay);
    }

    /// Remove a peer's overlay address when they disconnect.
    pub fn unregister_peer(&mut self, peer_id: &PeerId) {
        self.peer_overlays.remove(peer_id);
        self.peer_connections.remove(peer_id);
    }

    /// Notify observers of a threshold announcement.
    fn notify_observers(&self, peer_id: &PeerId, threshold: U256) {
        if let Some(overlay) = self.peer_overlays.get(peer_id) {
            for observer in &self.observers {
                observer.on_payment_threshold(overlay, threshold);
            }
        }
    }
}

impl NetworkBehaviour for PricingBehaviour {
    type ConnectionHandler = Handler;
    type ToSwarm = PricingEvent;

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(established) => {
                // Track this connection for the peer
                self.peer_connections
                    .entry(established.peer_id)
                    .or_default()
                    .push(established.connection_id);
            }
            FromSwarm::ConnectionClosed(closed) => {
                // Remove this connection from tracking
                if let Some(connections) = self.peer_connections.get_mut(&closed.peer_id) {
                    connections.retain(|&id| id != closed.connection_id);
                }
                // Clean up peer overlay if all connections are closed
                if closed.remaining_established == 0 {
                    self.unregister_peer(&closed.peer_id);
                }
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: HandlerEvent,
    ) {
        match event {
            HandlerEvent::ThresholdReceived { threshold } => {
                // Notify observers
                self.notify_observers(&peer_id, threshold);

                // Emit event for the swarm
                self.events
                    .push_back(ToSwarm::GenerateEvent(PricingEvent::ThresholdReceived {
                        peer_id,
                        connection_id,
                        threshold,
                    }));
            }
            HandlerEvent::ThresholdSent => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(PricingEvent::ThresholdSent {
                        peer_id,
                    }));
            }
            HandlerEvent::ThresholdTooLow { threshold, minimum } => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(PricingEvent::ThresholdTooLow {
                        peer_id,
                        threshold,
                        minimum,
                    }));
                // Note: The caller should handle disconnecting the peer
            }
            HandlerEvent::Error(error) => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(PricingEvent::Error {
                        peer_id,
                        error: error.to_string(),
                    }));
            }
        }
    }

    fn poll(
        &mut self,
        _: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }
        Poll::Pending
    }

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(Handler::new(self.config.handler_config.clone()))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(Handler::new(self.config.handler_config.clone()))
    }
}
