use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use libp2p::{
    swarm::{
        handler::{ConnectionEvent, FullyNegotiatedInbound, FullyNegotiatedOutbound},
        ConnectionHandler, ConnectionHandlerEvent, SubstreamProtocol,
    },
    Multiaddr, PeerId,
};

use crate::{
    HandshakeCommand, HandshakeConfig, HandshakeError, HandshakeEvent, HandshakeInfo,
    HandshakeProtocol, HandshakeState, HANDSHAKE_TIMEOUT,
};

pub struct HandshakeHandler<const N: u64> {
    config: Arc<HandshakeConfig<N>>,
    state: HandshakeState,
    queued_events: VecDeque<HandshakeEvent<N>>,
    pending_result:
        Option<Pin<Box<dyn Future<Output = Result<HandshakeInfo<N>, HandshakeError>> + Send>>>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
}

impl<const N: u64> HandshakeHandler<N> {
    pub fn new(config: Arc<HandshakeConfig<N>>, peer_id: PeerId, remote_addr: Multiaddr) -> Self {
        Self {
            config,
            state: HandshakeState::Idle,
            queued_events: VecDeque::new(),
            pending_result: None,
            peer_id,
            remote_addr,
        }
    }
}

impl<const N: u64> ConnectionHandler for HandshakeHandler<N> {
    type FromBehaviour = HandshakeCommand;
    type ToBehaviour = HandshakeEvent<N>;
    type InboundProtocol = HandshakeProtocol<N>;
    type OutboundProtocol = HandshakeProtocol<N>;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol> {
        SubstreamProtocol::new(
            HandshakeProtocol {
                config: self.config.clone(),
                peer_id: self.peer_id.clone(),
                remote_addr: self.remote_addr.clone(),
            },
            (),
        )
        .with_timeout(HANDSHAKE_TIMEOUT)
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            HandshakeCommand::StartHandshake => {
                if matches!(self.state, HandshakeState::Idle) {
                    self.state = HandshakeState::Start;
                }
            }
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        // First check for any queued events
        if let Some(event) = self.queued_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Then check pending handshake result
        if let Some(pending) = self.pending_result.as_mut() {
            match pending.as_mut().poll(cx) {
                Poll::Ready(Ok(info)) => {
                    self.pending_result = None;
                    self.state = HandshakeState::Completed;
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        HandshakeEvent::Completed(info),
                    ));
                }
                Poll::Ready(Err(e)) => {
                    self.pending_result = None;
                    self.state = HandshakeState::Failed;
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        HandshakeEvent::Failed(e),
                    ));
                }
                Poll::Pending => {}
            }
        }

        // Check if we need to initiate an outbound handshake
        match self.state {
            HandshakeState::Start => {
                self.state = HandshakeState::Handshaking;
                return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                    protocol: SubstreamProtocol::new(
                        HandshakeProtocol {
                            config: self.config.clone(),
                            peer_id: self.peer_id.clone(),
                            remote_addr: self.remote_addr.clone(),
                        },
                        (),
                    ),
                });
            }
            _ => {}
        }

        Poll::Pending
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<Self::InboundProtocol, Self::OutboundProtocol>,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: info,
                ..
            })
            | ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: info,
                ..
            }) => {
                self.state = HandshakeState::Completed;
                self.queued_events
                    .push_back(HandshakeEvent::Completed(info));
            }
            ConnectionEvent::DialUpgradeError(error) => {
                let handshake_error = HandshakeError::Protocol(error.error.to_string());
                self.state = HandshakeState::Failed;
                self.queued_events
                    .push_back(HandshakeEvent::Failed(handshake_error));
            }
            ConnectionEvent::ListenUpgradeError(error) => {
                let handshake_error = HandshakeError::Protocol(error.error.to_string());
                self.state = HandshakeState::Failed;
                self.queued_events
                    .push_back(HandshakeEvent::Failed(handshake_error));
            }
            _ => {}
        }
    }
}
