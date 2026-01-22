//! Connection handler for the pricing protocol.
//!
//! The handler manages pricing streams for a single connection. It:
//! - Waits for handshake completion before initiating pricing
//! - Sends our threshold via an outbound stream
//! - Receives peer's threshold via inbound stream
//! - When we receive a threshold, we trigger sending ours if we haven't yet

use std::{
    collections::VecDeque,
    task::{Context, Poll},
    time::Duration,
};

use alloy_primitives::U256;
use libp2p::swarm::{
    handler::{
        ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
        FullyNegotiatedOutbound,
    },
    SubstreamProtocol,
};

use crate::{
    codec::AnnouncePaymentThreshold,
    protocol::{PricingError, PricingInboundOutput, PricingOutboundOutput, PricingProtocol},
    MIN_PAYMENT_THRESHOLD,
};

/// Configuration for the pricing handler.
#[derive(Debug, Clone)]
pub struct Config {
    /// Payment threshold for full nodes.
    pub payment_threshold: U256,
    /// Payment threshold for light nodes.
    pub light_payment_threshold: U256,
    /// Minimum acceptable threshold from peers.
    pub min_payment_threshold: U256,
    /// Whether the local node is a full node.
    pub is_full_node: bool,
    /// Timeout for pricing protocol exchange.
    pub timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            payment_threshold: U256::from(crate::DEFAULT_PAYMENT_THRESHOLD),
            light_payment_threshold: U256::from(crate::DEFAULT_LIGHT_PAYMENT_THRESHOLD),
            min_payment_threshold: U256::from(MIN_PAYMENT_THRESHOLD),
            is_full_node: true,
            timeout: Duration::from_secs(5),
        }
    }
}

/// Commands sent from the behaviour to the handler.
#[derive(Debug)]
pub enum Command {
    /// Start the pricing exchange.
    ///
    /// This should be sent after handshake completion. The boolean indicates
    /// whether the peer is a full node (from handshake ack).
    StartPricing {
        /// Whether the peer is a full node.
        peer_is_full_node: bool,
    },
}

/// Events emitted by the pricing handler to the behaviour.
#[derive(Debug)]
pub enum Event {
    /// Successfully received a payment threshold from the peer.
    ThresholdReceived {
        /// The payment threshold announced by the peer.
        threshold: U256,
    },
    /// Successfully sent our payment threshold to the peer.
    ThresholdSent,
    /// The peer's threshold was below our minimum - they should be disconnected.
    ThresholdTooLow {
        /// The threshold they announced.
        threshold: U256,
        /// Our minimum requirement.
        minimum: U256,
    },
    /// Failed to complete the pricing exchange.
    Error(PricingError),
}

/// Handler for pricing protocol on a single connection.
pub struct Handler {
    /// Configuration.
    config: Config,
    /// Whether we've been commanded to start pricing.
    start_requested: bool,
    /// Whether we've initiated outbound pricing (only do it once per connection).
    outbound_requested: bool,
    /// Whether we've sent our threshold.
    threshold_sent: bool,
    /// Whether the peer is a full node (set when StartPricing command received).
    peer_is_full_node: bool,
    /// Pending events to emit.
    pending_events: VecDeque<Event>,
}

impl Handler {
    /// Create a new pricing handler.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            start_requested: false,
            outbound_requested: false,
            threshold_sent: false,
            peer_is_full_node: true, // Default assumption until told otherwise
            pending_events: VecDeque::new(),
        }
    }

    /// Get the threshold to announce based on whether the peer is a full node.
    fn threshold_for_peer(&self, peer_is_full_node: bool) -> U256 {
        if peer_is_full_node {
            self.config.payment_threshold
        } else {
            self.config.light_payment_threshold
        }
    }

    /// Process a received threshold from inbound.
    fn process_inbound(&mut self, output: PricingInboundOutput) {
        let threshold = output.peer_threshold.payment_threshold;

        // Check if threshold is acceptable
        if threshold < self.config.min_payment_threshold && threshold != U256::ZERO {
            self.pending_events.push_back(Event::ThresholdTooLow {
                threshold,
                minimum: self.config.min_payment_threshold,
            });
        } else {
            self.pending_events.push_back(Event::ThresholdReceived { threshold });
        }

        // If we haven't sent our threshold yet, we need to do so now.
        // This happens when the peer initiates pricing before we do.
        if !self.threshold_sent && !self.outbound_requested {
            // Mark that we should send our threshold
            self.start_requested = true;
        }
    }

    /// Process successful outbound (we sent our threshold).
    fn process_outbound(&mut self, _output: PricingOutboundOutput) {
        self.threshold_sent = true;
        self.pending_events.push_back(Event::ThresholdSent);
    }
}

impl ConnectionHandler for Handler {
    type FromBehaviour = Command;
    type ToBehaviour = Event;
    type InboundProtocol = PricingProtocol;
    type OutboundProtocol = PricingProtocol;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        // For inbound, use the known peer type if we have it, otherwise assume full node
        let threshold = self.threshold_for_peer(self.peer_is_full_node);
        SubstreamProtocol::new(
            PricingProtocol::new(AnnouncePaymentThreshold::new(threshold)),
            (),
        )
        .with_timeout(self.config.timeout)
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>>
    {
        // Check for pending events first
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Only request outbound stream after we've been commanded to start
        // (which happens after handshake completion)
        if self.start_requested && !self.outbound_requested {
            self.outbound_requested = true;
            let threshold = self.threshold_for_peer(self.peer_is_full_node);
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(
                    PricingProtocol::new(AnnouncePaymentThreshold::new(threshold)),
                    (),
                )
                .with_timeout(self.config.timeout),
            });
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            Command::StartPricing { peer_is_full_node } => {
                self.peer_is_full_node = peer_is_full_node;
                self.start_requested = true;
            }
        }
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: output,
                info: (),
            }) => {
                self.process_inbound(output);
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: output,
                info: (),
            }) => {
                self.process_outbound(output);
            }
            ConnectionEvent::DialUpgradeError(e) => {
                tracing::warn!("Pricing dial upgrade error: {:?}", e.error);
                self.pending_events
                    .push_back(Event::Error(PricingError::ConnectionClosed));
            }
            ConnectionEvent::ListenUpgradeError(e) => {
                tracing::warn!("Pricing listen upgrade error: {:?}", e.error);
                self.pending_events
                    .push_back(Event::Error(PricingError::ConnectionClosed));
            }
            _ => {}
        }
    }
}
