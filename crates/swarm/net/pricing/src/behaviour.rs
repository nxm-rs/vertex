//! `NetworkBehaviour` for the pricing protocol.
//!
//! Two orthogonal axes of configuration:
//!
//! * **Listen-only vs. announce-on-connect** ([`PricingRole`]). Bootnodes
//!   advertise the protocol but never dial a stream of their own; clients
//!   and full nodes both advertise AND queue an announce on every new
//!   connection. The interop requirement is satisfied as soon as we accept
//!   inbound streams, so the announce side is only needed when WE want our
//!   threshold tracked by the remote (i.e. when we participate in chunk
//!   accounting).
//! * **Observer for inbound thresholds** ([`PricingMode`]). Stub-mode
//!   discards them; full mode forwards them to a
//!   [`PaymentThresholdObserver`] that hooks into the accounting subsystem.

use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
};

use libp2p::{
    Multiaddr, PeerId,
    core::Endpoint,
    swarm::{
        ConnectionClosed, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour,
        NotifyHandler, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
};
use tracing::{debug, warn};

use crate::{
    AnnouncePaymentThreshold,
    handler::{PricingHandler, PricingHandlerCommand, PricingHandlerEvent},
    stub::{PaymentThresholdObserver, StubObserver, stub_announcement},
};

/// Maximum pending swarm events before we start dropping with a warning.
const MAX_PENDING_EVENTS: usize = 1024;

/// Behaviour role: whether we open an outbound stream to announce our own
/// threshold on every new connection. Bootnodes use [`Self::ListenOnly`];
/// nodes participating in chunk accounting use [`Self::Announcer`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum PricingRole {
    /// Listen-only: advertise the protocol so the remote's `ConnectIn`/`ConnectOut`
    /// succeeds, accept inbound threshold announcements, never dial an
    /// outbound stream of our own. Used by bootnodes.
    #[default]
    ListenOnly,
    /// Announce-on-connect: queue an outbound `AnnouncePaymentThreshold` to
    /// the remote on every new connection. Used by clients and full nodes
    /// that participate in chunk accounting.
    Announcer(AnnouncePaymentThreshold),
}

/// How received thresholds are routed.
///
/// `Stub` is the default for bootnodes — incoming thresholds are observed
/// (so we still emit a `PricingEvent::ThresholdReceived` for visibility) but
/// not forwarded to any accounting subsystem. `Full` accepts an
/// `Arc<dyn PaymentThresholdObserver>` that forwards thresholds into
/// accounting.
#[derive(Default)]
#[non_exhaustive]
pub enum PricingMode {
    /// Stub mode: observe inbound thresholds but do not feed an accounting
    /// subsystem.
    #[default]
    Stub,
    /// Full mode: forward inbound thresholds to the supplied observer.
    Full(Arc<dyn PaymentThresholdObserver>),
}

impl std::fmt::Debug for PricingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stub => f.write_str("Stub"),
            Self::Full(_) => f.write_str("Full(<observer>)"),
        }
    }
}

/// Events emitted by [`PricingBehaviour`].
#[derive(Debug)]
#[non_exhaustive]
pub enum PricingEvent {
    /// A peer announced its payment threshold.
    ThresholdReceived {
        peer: PeerId,
        threshold: AnnouncePaymentThreshold,
    },
    /// We sent our payment threshold to a peer.
    AnnouncementSent { peer: PeerId },
    /// An inbound stream failed. Informational; the connection is not torn
    /// down.
    InboundError { peer: PeerId, error: String },
    /// An outbound announcement failed. Informational; the connection is not
    /// torn down (peers without a pricing implementation must still keep
    /// the connection).
    OutboundError { peer: PeerId, error: String },
    /// An outbound announce was discarded because the behaviour-level event
    /// queue was full or the per-handler queue was full. Surfaced so
    /// operators see when the announce path silently failed to deliver our
    /// threshold to a peer.
    AnnouncementDropped { peer: PeerId },
}

/// `NetworkBehaviour` for the pricing protocol.
pub struct PricingBehaviour {
    /// Observer for received thresholds.
    observer: Arc<dyn PaymentThresholdObserver>,
    /// Role: listen-only or announcer.
    role: PricingRole,
    /// Queued swarm events.
    events: VecDeque<ToSwarm<PricingEvent, PricingHandlerCommand>>,
}

impl PricingBehaviour {
    /// Construct a behaviour with the given role and inbound mode.
    pub fn new(role: PricingRole, mode: PricingMode) -> Self {
        let observer: Arc<dyn PaymentThresholdObserver> = match mode {
            PricingMode::Stub => Arc::new(StubObserver),
            PricingMode::Full(obs) => obs,
        };
        Self {
            observer,
            role,
            events: VecDeque::new(),
        }
    }

    /// Listen-only stub for bootnodes. Advertises the protocol and accepts
    /// inbound threshold announcements but never dials a stream of its own.
    pub fn new_bootnode() -> Self {
        Self::new(PricingRole::ListenOnly, PricingMode::Stub)
    }

    /// Announce-on-connect for non-bootnode roles. `threshold` is the value
    /// announced to every newly-established peer; the inbound mode controls
    /// how received thresholds are routed.
    pub fn new_announcer(threshold: u64, mode: PricingMode) -> Self {
        Self::new(PricingRole::Announcer(stub_announcement(threshold)), mode)
    }

    /// The protocol name this behaviour speaks.
    pub const fn protocol_name() -> &'static str {
        crate::PROTOCOL_NAME
    }

    fn push_event(&mut self, event: ToSwarm<PricingEvent, PricingHandlerCommand>) {
        if self.events.len() >= MAX_PENDING_EVENTS {
            warn!("Pricing behaviour event queue full, dropping event");
            return;
        }
        self.events.push_back(event);
    }

    /// Queue an outbound announce on a freshly-established connection.
    /// No-op for [`PricingRole::ListenOnly`].
    fn announce_to(&mut self, peer_id: PeerId) {
        let PricingRole::Announcer(threshold) = &self.role else {
            return;
        };
        debug!(%peer_id, threshold = %threshold.payment_threshold, "Pricing: queuing announcement to peer");
        let threshold = threshold.clone();
        self.push_event(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::Any,
            event: PricingHandlerCommand::Announce(threshold),
        });
    }
}

impl NetworkBehaviour for PricingBehaviour {
    type ConnectionHandler = PricingHandler;
    type ToSwarm = PricingEvent;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.announce_to(peer);
        Ok(PricingHandler::new())
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: libp2p::core::transport::PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.announce_to(peer);
        Ok(PricingHandler::new())
    }

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        if let FromSwarm::ConnectionClosed(ConnectionClosed { peer_id, .. }) = event {
            debug!(%peer_id, "Pricing: connection closed");
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        _connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {
            PricingHandlerEvent::ThresholdReceived(threshold) => {
                self.observer
                    .record_threshold(peer_id, threshold.payment_threshold);
                self.push_event(ToSwarm::GenerateEvent(PricingEvent::ThresholdReceived {
                    peer: peer_id,
                    threshold,
                }));
            }
            PricingHandlerEvent::AnnouncementSent => {
                self.push_event(ToSwarm::GenerateEvent(PricingEvent::AnnouncementSent {
                    peer: peer_id,
                }));
            }
            PricingHandlerEvent::InboundError(error) => {
                self.push_event(ToSwarm::GenerateEvent(PricingEvent::InboundError {
                    peer: peer_id,
                    error,
                }));
            }
            PricingHandlerEvent::OutboundError(error) => {
                self.push_event(ToSwarm::GenerateEvent(PricingEvent::OutboundError {
                    peer: peer_id,
                    error,
                }));
            }
            PricingHandlerEvent::OutboundDropped => {
                self.push_event(ToSwarm::GenerateEvent(PricingEvent::AnnouncementDropped {
                    peer: peer_id,
                }));
            }
        }
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_name_matches_const() {
        assert_eq!(PricingBehaviour::protocol_name(), crate::PROTOCOL_NAME);
    }

    #[test]
    fn bootnode_constructor_is_listen_only() {
        // Bootnodes must not announce: they have no payment state of their own.
        let b = PricingBehaviour::new_bootnode();
        assert!(matches!(b.role, PricingRole::ListenOnly));
    }

    #[test]
    fn announcer_constructor_carries_threshold() {
        let b = PricingBehaviour::new_announcer(13_500_000, PricingMode::Stub);
        assert!(matches!(b.role, PricingRole::Announcer(_)));
    }
}
