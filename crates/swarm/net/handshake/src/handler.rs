//! Per-connection handler for handshake protocol.

use std::{
    sync::Arc,
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
use vertex_swarm_api::SwarmIdentity;

use crate::{
    AddressProvider, HANDSHAKE_TIMEOUT, HandshakeError, HandshakeInfo, PROTOCOL,
    protocol::HandshakeProtocol,
};

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
    ) -> Self {
        Self {
            config,
            identity,
            peer_id,
            remote_addr,
            address_provider,
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
    ) -> Self {
        Self {
            config,
            identity,
            peer_id,
            remote_addr,
            address_provider,
            state: State::Pending,
            pending_event: None,
            should_initiate: true,
            outbound_pending: false,
        }
    }

    fn make_upgrade(&self) -> HandshakeUpgrade<I, A> {
        HandshakeUpgrade {
            identity: self.identity.clone(),
            peer_id: self.peer_id,
            remote_addr: self.remote_addr.clone(),
            address_provider: self.address_provider.clone(),
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
        SubstreamProtocol::new(self.make_upgrade(), ()).with_timeout(self.config.timeout)
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
                protocol: SubstreamProtocol::new(self.make_upgrade(), ())
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
                self.state = State::Failed;
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
    purpose: &'static str,
}

impl<I, A> Clone for HandshakeUpgrade<I, A> {
    fn clone(&self) -> Self {
        Self {
            identity: self.identity.clone(),
            peer_id: self.peer_id,
            remote_addr: self.remote_addr.clone(),
            address_provider: self.address_provider.clone(),
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
        let additional_addrs = self.address_provider.addresses_for_peer(&self.remote_addr);
        let local_peer_id = self.address_provider.local_peer_id().copied();

        let mut protocol = HandshakeProtocol::new(
            self.identity,
            self.peer_id,
            self.remote_addr,
            additional_addrs,
            self.purpose,
        );
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
