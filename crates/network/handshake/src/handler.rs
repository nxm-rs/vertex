use std::{
    collections::VecDeque,
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
use vertex_node_core::args::NodeCommand;

use crate::{
    HandshakeCommand, HandshakeError, HandshakeEvent, HandshakeProtocol, HandshakeState,
    HANDSHAKE_TIMEOUT,
};

pub struct HandshakeHandler<const N: u64> {
    peer_id: PeerId,
    remote_addr: Multiaddr,
    config: Arc<NodeCommand>,
    state: HandshakeState,
    pending_events: VecDeque<ConnectionHandlerEvent<HandshakeProtocol<N>, (), HandshakeEvent<N>>>,
}

impl<const N: u64> HandshakeHandler<N> {
    pub fn new(config: Arc<NodeCommand>, peer_id: PeerId, remote_addr: &Multiaddr) -> Self {
        Self {
            peer_id,
            remote_addr: remote_addr.clone(),
            config,
            state: HandshakeState::Idle,
            pending_events: VecDeque::new(),
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
                    self.state = HandshakeState::Handshaking;
                    self.pending_events.push_back(
                        ConnectionHandlerEvent::OutboundSubstreamRequest {
                            protocol: self.listen_protocol(),
                        },
                    )
                }
            }
        }
    }

    fn connection_keep_alive(&self) -> bool {
        !matches!(self.state, HandshakeState::Failed)
    }

    fn poll(
        &mut self,
        _: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        // First check for any queued events
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(event);
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
                self.pending_events
                    .push_back(ConnectionHandlerEvent::NotifyBehaviour(
                        HandshakeEvent::Completed(info),
                    ));
            }
            ConnectionEvent::DialUpgradeError(error) => {
                let handshake_error = HandshakeError::Protocol(error.error.to_string());
                self.state = HandshakeState::Failed;
                self.pending_events
                    .push_back(ConnectionHandlerEvent::NotifyBehaviour(
                        HandshakeEvent::Failed(handshake_error),
                    ));
            }
            ConnectionEvent::ListenUpgradeError(error) => {
                let handshake_error = HandshakeError::Protocol(error.error.to_string());
                self.state = HandshakeState::Failed;
                self.pending_events
                    .push_back(ConnectionHandlerEvent::NotifyBehaviour(
                        HandshakeEvent::Failed(handshake_error),
                    ));
            }
            _ => {}
        }
    }
}
