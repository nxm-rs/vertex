//! Per-connection handler for the pricing protocol.
//!
//! Accepts inbound `/swarm/pricing/1.0.0/pricing` streams and emits the
//! received [`AnnouncePaymentThreshold`] to the behaviour. The handler also
//! supports opening an outbound stream to announce our own threshold; the
//! behaviour decides whether to issue such announcements (bootnodes do not,
//! client and full nodes do).
//!
//! Outbound failures are reported as events but never tear down the
//! connection: a peer without a pricing implementation must still keep the
//! connection open.

use std::{
    collections::VecDeque,
    task::{Context, Poll},
    time::Duration,
};

use futures_bounded::Timeout;
use libp2p::{
    InboundUpgrade,
    core::upgrade::ReadyUpgrade,
    swarm::{
        StreamProtocol, SubstreamProtocol,
        handler::{
            ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
            FullyNegotiatedOutbound, ListenUpgradeError,
        },
    },
};
use tracing::{debug, warn};
use vertex_swarm_net_headers::{Inbound, ProtocolError};

use crate::{
    AnnouncePaymentThreshold, PROTOCOL_NAME, PricingOutboundProtocol, outbound,
    protocol::PricingInner,
};

/// Timeout for inbound stream processing.
const STREAM_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum concurrent inbound pricing streams per connection.
const MAX_CONCURRENT_INBOUND: usize = 2;
/// Maximum queued outbound announcements per connection. Sized comfortably
/// above the steady-state of "one announce per fresh connection" so a burst
/// does not silently drop the one event the announce path exists to deliver.
const MAX_PENDING_OUTBOUND: usize = 16;
/// Outbound stream operation timeout.
const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(30);

const PRICING_STREAM_PROTOCOL: StreamProtocol = StreamProtocol::new(PROTOCOL_NAME);

/// Commands from behaviour to handler.
#[derive(Debug)]
pub enum PricingHandlerCommand {
    /// Open an outbound stream and announce our threshold.
    Announce(AnnouncePaymentThreshold),
}

/// Events from handler to behaviour.
#[derive(Debug)]
#[non_exhaustive]
pub enum PricingHandlerEvent {
    /// Remote announced its payment threshold.
    ThresholdReceived(AnnouncePaymentThreshold),
    /// Our outbound announcement completed.
    AnnouncementSent,
    /// An inbound stream failed.
    InboundError(String),
    /// An outbound stream failed. Logged at warn; **not** propagated as a
    /// behaviour-level error.
    OutboundError(String),
    /// An outbound announce was discarded because the per-handler queue was
    /// full. Surfaced so operators see when the announce path silently
    /// failed to deliver.
    OutboundDropped,
}

/// Marker info for outbound substream requests.
#[derive(Debug, Clone, Copy, Default)]
pub struct PricingOutboundInfo;

/// Per-connection handler for pricing.
pub struct PricingHandler {
    /// Bounded set of inbound stream futures.
    inbound: futures_bounded::FuturesSet<Result<AnnouncePaymentThreshold, ProtocolError>>,
    /// Pending outbound announcements.
    pending_outbound: VecDeque<AnnouncePaymentThreshold>,
    /// Whether an outbound substream request is in flight.
    outbound_in_flight: bool,
    /// Events to emit to the behaviour.
    pending_events: VecDeque<PricingHandlerEvent>,
}

impl PricingHandler {
    /// Construct a new pricing handler.
    pub fn new() -> Self {
        Self {
            inbound: futures_bounded::FuturesSet::new(STREAM_TIMEOUT, MAX_CONCURRENT_INBOUND),
            pending_outbound: VecDeque::new(),
            outbound_in_flight: false,
            pending_events: VecDeque::new(),
        }
    }
}

impl Default for PricingHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionHandler for PricingHandler {
    type FromBehaviour = PricingHandlerCommand;
    type ToBehaviour = PricingHandlerEvent;
    type InboundProtocol = ReadyUpgrade<StreamProtocol>;
    type OutboundProtocol = PricingOutboundProtocol;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = PricingOutboundInfo;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(ReadyUpgrade::new(PRICING_STREAM_PROTOCOL), ())
    }

    fn connection_keep_alive(&self) -> bool {
        // Keep alive only while pricing has actual work to do; topology
        // owns the long-term keep-alive decision once we have nothing
        // pending or in-flight.
        self.outbound_in_flight
            || !self.pending_outbound.is_empty()
            || !self.pending_events.is_empty()
            || !self.inbound.is_empty()
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        // Drain inbound unconditionally so its waker is re-registered with
        // the current context; pushing the result through `pending_events`
        // means a Ready return from an earlier branch does not leave a
        // finished inbound stream un-noticed.
        if let Poll::Ready(result) = self.inbound.poll_unpin(cx) {
            let event = match result {
                Ok(Ok(threshold)) => PricingHandlerEvent::ThresholdReceived(threshold),
                Ok(Err(err)) => {
                    warn!(error = %err, "Pricing inbound stream error");
                    PricingHandlerEvent::InboundError(err.to_string())
                }
                Err(Timeout { .. }) => {
                    warn!("Pricing inbound stream timed out");
                    PricingHandlerEvent::InboundError("inbound timeout".to_string())
                }
            };
            self.pending_events.push_back(event);
        }

        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        if !self.outbound_in_flight
            && let Some(threshold) = self.pending_outbound.pop_front()
        {
            self.outbound_in_flight = true;
            debug!(threshold = %threshold.payment_threshold, "Pricing: opening outbound stream");
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(outbound(threshold), PricingOutboundInfo)
                    .with_timeout(OUTBOUND_TIMEOUT),
            });
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            PricingHandlerCommand::Announce(threshold) => {
                if self.pending_outbound.len() >= MAX_PENDING_OUTBOUND {
                    warn!("Dropping pricing announcement: pending queue full");
                    self.pending_events
                        .push_back(PricingHandlerEvent::OutboundDropped);
                    return;
                }
                self.pending_outbound.push_back(threshold);
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
                protocol: stream,
                ..
            }) => {
                if self
                    .inbound
                    .try_push(async move {
                        let upgrade = Inbound::new(PricingInner);
                        upgrade.upgrade_inbound(stream, PROTOCOL_NAME).await
                    })
                    .is_err()
                {
                    warn!("Dropping inbound pricing stream: at capacity");
                    self.pending_events
                        .push_back(PricingHandlerEvent::InboundError(
                            "inbound stream dropped: handler at capacity".to_string(),
                        ));
                }
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound { .. }) => {
                self.outbound_in_flight = false;
                debug!("Pricing: outbound announcement complete");
                self.pending_events
                    .push_back(PricingHandlerEvent::AnnouncementSent);
            }
            ConnectionEvent::DialUpgradeError(error) => {
                // Never propagate as a hard error: peers without a pricing
                // implementation must still keep the connection.
                self.outbound_in_flight = false;
                let msg = error.error.to_string();
                warn!(error = %msg, "Pricing outbound upgrade failed (ignored)");
                self.pending_events
                    .push_back(PricingHandlerEvent::OutboundError(msg));
            }
            ConnectionEvent::ListenUpgradeError(ListenUpgradeError { error, .. }) => {
                // Surface inbound upgrade failures so operators see when an
                // inbound dial from a peer fails libp2p-side negotiation,
                // rather than silently swallowing the event.
                let msg = error.to_string();
                warn!(error = %msg, "Pricing inbound upgrade failed");
                self.pending_events
                    .push_back(PricingHandlerEvent::InboundError(msg));
            }
            _ => {}
        }
    }
}
