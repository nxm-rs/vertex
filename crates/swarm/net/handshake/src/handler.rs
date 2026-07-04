//! Per-connection handler for handshake protocol.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use futures::future::BoxFuture;
use libp2p::{
    InboundUpgrade, Multiaddr, OutboundUpgrade, PeerId, Stream,
    core::UpgradeInfo,
    swarm::{
        StreamUpgradeError, SubstreamProtocol,
        handler::{
            ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
            FullyNegotiatedOutbound,
        },
    },
};
use tracing::{debug, warn};
use vertex_metrics::labels::direction;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer::SwarmPeer;

use crate::{
    AddressProvider, ConnectionDirection, HANDSHAKE_TIMEOUT, HandshakeError, HandshakeInfo,
    PROTOCOL, SharedAdmissionControl, metrics::record_unexpected_exchange,
    protocol::HandshakeProtocol,
};

/// Admission of inbound handshake exchanges on one connection.
///
/// The exchange runs once per connection: a dialer connection never accepts
/// an inbound exchange, and a listener connection accepts only the first
/// substream to claim its slot. A denied substream fails the upgrade before
/// any frame is read.
#[derive(Clone)]
enum InboundExchangeGate {
    /// We dialed this connection: no inbound exchange is ever legitimate.
    Dialer,
    /// We listened: the first claim wins, every later attempt is denied.
    Listener { claimed: Arc<AtomicBool> },
}

impl InboundExchangeGate {
    fn listener() -> Self {
        Self::Listener {
            claimed: Arc::default(),
        }
    }

    /// Claim the connection's single inbound exchange; `false` denies.
    fn try_claim(&self) -> bool {
        match self {
            Self::Dialer => false,
            Self::Listener { claimed } => !claimed.swap(true, Ordering::AcqRel),
        }
    }

    /// Metric label for a denial: which side of the connection we play.
    fn direction_label(&self) -> &'static str {
        match self {
            Self::Dialer => direction::OUTBOUND,
            Self::Listener { .. } => direction::INBOUND,
        }
    }
}

/// Configuration for handshake handler.
#[derive(Debug, Clone)]
pub struct HandshakeConfig {
    /// Timeout for handshake protocol.
    pub timeout: Duration,
    /// Label for metrics to distinguish handshake contexts (e.g. "topology" vs "verifier").
    pub purpose: &'static str,
}

impl HandshakeConfig {
    /// Create a new config with the given purpose label.
    pub fn new(purpose: &'static str) -> Self {
        Self {
            timeout: HANDSHAKE_TIMEOUT,
            purpose,
        }
    }
}

/// Commands from behaviour to handler.
#[derive(Debug)]
pub enum HandshakeCommand {
    /// Initiate outbound handshake with resolved address.
    Initiate(Multiaddr),
}

/// Events from handler to behaviour.
pub enum HandshakeHandlerEvent {
    /// Handshake completed successfully.
    Completed { info: Box<HandshakeInfo> },
    /// Handshake failed.
    Failed { error: HandshakeError },
}

impl std::fmt::Debug for HandshakeHandlerEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed { .. } => f.debug_struct("Completed").finish_non_exhaustive(),
            Self::Failed { error, .. } => f.debug_struct("Failed").field("error", error).finish(),
        }
    }
}

/// Handler state.
#[derive(Debug)]
enum State {
    /// Waiting for handshake to start or complete.
    Pending,
    /// Handshake in progress.
    InProgress,
    /// Handshake completed successfully.
    Completed,
    /// Handshake failed.
    Failed,
}

/// Per-connection handler for handshake protocol only.
pub struct HandshakeHandler<I, A> {
    config: Arc<HandshakeConfig>,
    identity: Arc<I>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    address_provider: Arc<A>,
    /// Admission gate forwarded into [`HandshakeProtocol`].
    admission_control: SharedAdmissionControl,
    /// Pre-signed self record from the behaviour cache, reused across
    /// handshakes with an unchanged advertised address set. `None` when the
    /// advertised set is empty, in which case the protocol signs a last-resort
    /// record over the peer-observed address.
    self_record: Option<SwarmPeer>,
    /// Once-per-connection admission for inbound exchanges, shared with every
    /// upgrade this handler constructs.
    inbound_gate: InboundExchangeGate,
    state: State,
    pending_event: Option<HandshakeHandlerEvent>,
    should_initiate: bool,
    outbound_pending: bool,
}

impl<I, A> HandshakeHandler<I, A>
where
    I: SwarmIdentity + 'static,
    A: AddressProvider + 'static,
{
    /// Create a new handler for inbound connection.
    pub fn new_inbound(
        config: Arc<HandshakeConfig>,
        identity: Arc<I>,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        address_provider: Arc<A>,
        admission_control: SharedAdmissionControl,
        self_record: Option<SwarmPeer>,
    ) -> Self {
        Self {
            config,
            identity,
            peer_id,
            remote_addr,
            address_provider,
            admission_control,
            self_record,
            inbound_gate: InboundExchangeGate::listener(),
            state: State::Pending,
            pending_event: None,
            should_initiate: false,
            outbound_pending: false,
        }
    }

    /// Create a new handler for outbound connection.
    pub fn new_outbound(
        config: Arc<HandshakeConfig>,
        identity: Arc<I>,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        address_provider: Arc<A>,
        admission_control: SharedAdmissionControl,
        self_record: Option<SwarmPeer>,
    ) -> Self {
        Self {
            config,
            identity,
            peer_id,
            remote_addr,
            address_provider,
            admission_control,
            self_record,
            inbound_gate: InboundExchangeGate::Dialer,
            state: State::Pending,
            pending_event: None,
            should_initiate: true,
            outbound_pending: false,
        }
    }

    fn make_upgrade(&self, direction: ConnectionDirection) -> HandshakeUpgrade<I, A> {
        HandshakeUpgrade {
            identity: self.identity.clone(),
            peer_id: self.peer_id,
            remote_addr: self.remote_addr.clone(),
            address_provider: self.address_provider.clone(),
            admission_control: self.admission_control.clone(),
            self_record: self.self_record.clone(),
            inbound_gate: self.inbound_gate.clone(),
            direction,
            purpose: self.config.purpose,
        }
    }
}

impl<I, A> ConnectionHandler for HandshakeHandler<I, A>
where
    I: SwarmIdentity + 'static,
    A: AddressProvider + 'static,
{
    type FromBehaviour = HandshakeCommand;
    type ToBehaviour = HandshakeHandlerEvent;
    type InboundProtocol = HandshakeUpgrade<I, A>;
    type OutboundProtocol = HandshakeUpgrade<I, A>;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(self.make_upgrade(ConnectionDirection::Inbound), ())
            .with_timeout(self.config.timeout)
    }

    fn connection_keep_alive(&self) -> bool {
        !matches!(self.state, State::Failed)
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        if let Some(event) = self.pending_event.take() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        if self.should_initiate && !self.outbound_pending && matches!(self.state, State::Pending) {
            self.should_initiate = false;
            self.outbound_pending = true;
            self.state = State::InProgress;
            debug!(peer_id = %self.peer_id, "Initiating outbound handshake");
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(
                    self.make_upgrade(ConnectionDirection::Outbound),
                    (),
                )
                .with_timeout(self.config.timeout),
            });
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            HandshakeCommand::Initiate(addr) => {
                if matches!(self.state, State::Pending) && !self.outbound_pending {
                    self.remote_addr = addr;
                    self.should_initiate = true;
                    debug!(peer_id = %self.peer_id, "Handshake will use resolved address");
                }
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
                protocol: info,
                ..
            }) => {
                debug!(peer_id = %self.peer_id, "Inbound handshake completed");
                self.state = State::Completed;
                self.pending_event = Some(HandshakeHandlerEvent::Completed {
                    info: Box::new(info),
                });
            }

            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: info,
                ..
            }) => {
                self.outbound_pending = false;
                debug!(peer_id = %self.peer_id, "Outbound handshake completed");
                self.state = State::Completed;
                self.pending_event = Some(HandshakeHandlerEvent::Completed {
                    info: Box::new(info),
                });
            }

            ConnectionEvent::DialUpgradeError(error) => {
                self.outbound_pending = false;
                warn!(peer_id = %self.peer_id, "Outbound handshake failed: {}", error.error);
                self.state = State::Failed;
                let error = extract_error(error.error);
                self.pending_event = Some(HandshakeHandlerEvent::Failed { error });
            }

            ConnectionEvent::ListenUpgradeError(error) => {
                warn!(peer_id = %self.peer_id, "Inbound handshake failed: {}", error.error);
                // A denied extra exchange is the violating substream's failure,
                // not this connection's handshake outcome (which may already be
                // Completed); the behaviour drops the peer on the event.
                if !matches!(error.error, HandshakeError::UnexpectedExchange) {
                    self.state = State::Failed;
                }
                self.pending_event = Some(HandshakeHandlerEvent::Failed { error: error.error });
            }

            _ => {}
        }
    }
}

fn extract_error(error: StreamUpgradeError<HandshakeError>) -> HandshakeError {
    match error {
        StreamUpgradeError::Timeout => HandshakeError::Timeout,
        StreamUpgradeError::Io(e) => HandshakeError::Io(e),
        StreamUpgradeError::Apply(e) => e,
        StreamUpgradeError::NegotiationFailed => {
            HandshakeError::UpgradeError("protocol negotiation failed".into())
        }
    }
}

/// libp2p protocol upgrade that delegates to `HandshakeProtocol`.
///
/// Injects addresses from `AddressProvider` before running the handshake exchange.
pub struct HandshakeUpgrade<I, A> {
    identity: Arc<I>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    address_provider: Arc<A>,
    /// Forwarded into [`HandshakeProtocol`] so admission control can run
    /// before the local side commits to the final exchange message.
    admission_control: SharedAdmissionControl,
    /// Pre-signed self record from the behaviour cache. `None` means the
    /// advertised set was empty and the protocol must do the last-resort
    /// observed-address sign.
    self_record: Option<SwarmPeer>,
    /// Handler-shared once-per-connection admission for inbound exchanges.
    inbound_gate: InboundExchangeGate,
    /// Direction this upgrade was created for; drives which arm of the
    /// protocol runs and which side the admission gate sees.
    direction: ConnectionDirection,
    purpose: &'static str,
}

impl<I, A> Clone for HandshakeUpgrade<I, A> {
    fn clone(&self) -> Self {
        Self {
            identity: self.identity.clone(),
            peer_id: self.peer_id,
            remote_addr: self.remote_addr.clone(),
            address_provider: self.address_provider.clone(),
            admission_control: self.admission_control.clone(),
            self_record: self.self_record.clone(),
            inbound_gate: self.inbound_gate.clone(),
            direction: self.direction,
            purpose: self.purpose,
        }
    }
}

impl<I, A> UpgradeInfo for HandshakeUpgrade<I, A>
where
    I: SwarmIdentity + 'static,
    A: AddressProvider + 'static,
{
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL)
    }
}

impl<I, A> HandshakeUpgrade<I, A>
where
    I: SwarmIdentity + 'static,
    A: AddressProvider + 'static,
{
    fn build_protocol(self) -> HandshakeProtocol<Arc<I>> {
        let local_peer_id = self.address_provider.local_peer_id().copied();

        let mut protocol = HandshakeProtocol::new(
            self.identity,
            self.peer_id,
            self.remote_addr,
            self.self_record,
            self.purpose,
        )
        .with_admission_control(self.admission_control, self.direction);
        if let Some(local_peer_id) = local_peer_id {
            protocol = protocol.with_local_peer_id(local_peer_id);
        }
        protocol
    }
}

impl<I, A> InboundUpgrade<Stream> for HandshakeUpgrade<I, A>
where
    I: SwarmIdentity + 'static,
    A: AddressProvider + 'static,
{
    type Output = HandshakeInfo;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        // Gate before any frame is read: a denied substream is dropped
        // without spending signature-recovery work on it.
        if !self.inbound_gate.try_claim() {
            record_unexpected_exchange(self.inbound_gate.direction_label(), self.purpose);
            drop(socket);
            return Box::pin(std::future::ready(Err(HandshakeError::UnexpectedExchange)));
        }
        Box::pin(self.build_protocol().handle_inbound(socket))
    }
}

impl<I, A> OutboundUpgrade<Stream> for HandshakeUpgrade<I, A>
where
    I: SwarmIdentity + 'static,
    A: AddressProvider + 'static,
{
    type Output = HandshakeInfo;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        Box::pin(self.build_protocol().handle_outbound(socket))
    }
}

#[cfg(test)]
mod tests {
    use libp2p::swarm::handler::ListenUpgradeError;
    use vertex_swarm_peer::SwarmNodeType;
    use vertex_swarm_test_utils::{test_identity_arc, test_swarm_peer};

    use super::*;
    use crate::{NoAddresses, default_admission_control};

    #[test]
    fn dialer_gate_denies_every_inbound_exchange() {
        let gate = InboundExchangeGate::Dialer;
        assert!(!gate.try_claim());
        assert!(!gate.try_claim());
    }

    #[test]
    fn listener_gate_admits_exactly_one_exchange() {
        let gate = InboundExchangeGate::listener();
        assert!(gate.try_claim());
        assert!(!gate.try_claim());
        assert!(!gate.try_claim());
    }

    #[test]
    fn listener_gate_clones_share_the_claim() {
        // `listen_protocol` constructs one upgrade per inbound substream, so
        // the clones must contend for the same slot.
        let gate = InboundExchangeGate::listener();
        let clone = gate.clone();
        assert!(clone.try_claim());
        assert!(!gate.try_claim());
    }

    fn listener_handler() -> HandshakeHandler<impl SwarmIdentity + 'static, NoAddresses> {
        HandshakeHandler::new_inbound(
            Arc::new(HandshakeConfig::new("test")),
            test_identity_arc(),
            PeerId::random(),
            "/ip4/127.0.0.1/tcp/1634".parse().expect("valid multiaddr"),
            Arc::new(NoAddresses),
            default_admission_control(),
            None,
        )
    }

    fn completed_info(peer_id: PeerId) -> HandshakeInfo {
        HandshakeInfo {
            peer_id,
            swarm_peer: test_swarm_peer(1),
            node_type: SwarmNodeType::Client,
            welcome_message: String::new(),
            observed_multiaddr: "/ip4/127.0.0.1/tcp/1634".parse().expect("valid multiaddr"),
        }
    }

    #[test]
    fn denied_exchange_does_not_clobber_a_completed_handshake() {
        let mut handler = listener_handler();
        let mut cx = Context::from_waker(std::task::Waker::noop());

        handler.on_connection_event(ConnectionEvent::FullyNegotiatedInbound(
            FullyNegotiatedInbound {
                protocol: completed_info(PeerId::random()),
                info: (),
            },
        ));
        assert!(matches!(
            handler.poll(&mut cx),
            Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                HandshakeHandlerEvent::Completed { .. }
            ))
        ));

        handler.on_connection_event(ConnectionEvent::ListenUpgradeError(ListenUpgradeError {
            info: (),
            error: HandshakeError::UnexpectedExchange,
        }));

        // The connection's own handshake outcome stands; the violation still
        // surfaces so the behaviour can drop the peer.
        assert!(handler.connection_keep_alive());
        assert!(matches!(
            handler.poll(&mut cx),
            Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                HandshakeHandlerEvent::Failed {
                    error: HandshakeError::UnexpectedExchange
                }
            ))
        ));
    }

    #[test]
    fn ordinary_listen_failure_still_fails_the_handshake() {
        let mut handler = listener_handler();

        handler.on_connection_event(ConnectionEvent::ListenUpgradeError(ListenUpgradeError {
            info: (),
            error: HandshakeError::NetworkIdMismatch,
        }));

        assert!(!handler.connection_keep_alive());
    }
}
