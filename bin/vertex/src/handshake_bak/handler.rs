// handler.rs
use std::{
    collections::VecDeque,
    task::{Context, Poll},
    time::Duration,
};

use futures::prelude::*;
use libp2p::swarm::{
    handler::{ConnectionEvent, FullyNegotiatedInbound, FullyNegotiatedOutbound},
    ConnectionHandler, ConnectionHandlerEvent, SubstreamProtocol,
};
use libp2p::{
    core::{Endpoint, Multiaddr},
    PeerId,
};

use super::{
    behaviour::{Config, HandshakeInfo},
    protocol::{HandshakeInStreamSink, HandshakeMessage, HandshakeOutStreamSink, ProtocolConfig},
    HandshakeError,
};

#[derive(Debug)]
pub enum HandlerEvent {
    HandshakeCompleted(HandshakeInfo),
    HandshakeFailed(HandshakeError),
}

#[derive(Debug)]
pub enum HandlerIn {
    StartHandshake,
}

pub struct Handler {
    /// Configuration
    config: Config,
    /// Protocol configuration
    protocol_config: ProtocolConfig,
    /// Remote address
    remote_addr: Multiaddr,
    /// Connection role
    endpoint: Endpoint,
    /// Current state
    state: State,
    /// Pending events to emit
    pending_events: VecDeque<HandlerEvent>,
}

#[derive(Debug)]
enum State {
    /// Waiting to start handshake
    Idle,
    /// Handshake in progress
    Handshaking,
    /// Handshake completed
    Completed,
    /// Handshake failed
    Failed,
}

impl Handler {
    pub fn new(config: Config, remote_addr: Multiaddr, endpoint: Endpoint) -> Self {
        Self {
            protocol_config: config.protocol_config.clone(),
            config,
            remote_addr,
            endpoint,
            state: State::Idle,
            pending_events: VecDeque::new(),
        }
    }

    fn on_handshake_completed(&mut self, info: HandshakeInfo) {
        self.state = State::Completed;
        self.pending_events
            .push_back(HandlerEvent::HandshakeCompleted(info));
    }

    fn on_handshake_failed(&mut self, error: HandshakeError) {
        self.state = State::Failed;
        self.pending_events
            .push_back(HandlerEvent::HandshakeFailed(error));
    }
}

impl ConnectionHandler for Handler {
    type FromBehaviour = HandlerIn;
    type ToBehaviour = HandlerEvent;
    type InboundProtocol = ProtocolConfig;
    type OutboundProtocol = ProtocolConfig;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol> {
        SubstreamProtocol::new(self.protocol_config.clone(), ())
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            HandlerIn::StartHandshake => {
                if matches!(self.state, State::Idle) {
                    self.state = State::Handshaking;
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
                protocol: stream,
                ..
            }) => {
                // Handle inbound handshake
                match handle_inbound_handshake(stream, &self.config, &self.remote_addr).await {
                    Ok(info) => self.on_handshake_completed(info),
                    Err(error) => self.on_handshake_failed(error),
                }
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: stream,
                ..
            }) => {
                // Handle outbound handshake
                match handle_outbound_handshake(stream, &self.config, &self.remote_addr).await {
                    Ok(info) => self.on_handshake_completed(info),
                    Err(error) => self.on_handshake_failed(error),
                }
            }
            ConnectionEvent::DialUpgradeError(error) => {
                self.on_handshake_failed(HandshakeError::from(error.error));
            }
            _ => {}
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        // Handle pending events first
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Start handshake if needed
        if matches!(self.state, State::Handshaking) {
            self.state = State::Handshaking;
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(self.protocol_config.clone(), ()),
            });
        }

        Poll::Pending
    }
}

async fn handle_inbound_handshake(
    mut stream: HandshakeInStreamSink<impl AsyncRead + AsyncWrite + Unpin>,
    config: &Config,
    remote_addr: &Multiaddr,
) -> Result<HandshakeInfo, HandshakeError> {
    // Read SYN
    let syn = match stream.next().await {
        Some(Ok(HandshakeMessage::Syn(syn))) => syn,
        _ => return Err(HandshakeError::InvalidMessage("Expected SYN".into())),
    };

    // Create observed underlay address
    let observed_underlay = remote_addr.to_vec();

    // Send SYNACK
    let syn_ack = SynAck {
        Syn: Some(Syn {
            ObservedUnderlay: Cow::Owned(observed_underlay),
        }),
        Ack: Some(Ack {
            NetworkID: config.network_id,
            FullNode: config.full_node,
            WelcomeMessage: Cow::Borrowed(&config.welcome_message),
            ..Default::default()
        }),
    };

    stream.send(HandshakeMessage::SynAck(syn_ack)).await?;

    // Read ACK
    let ack = match stream.next().await {
        Some(Ok(HandshakeMessage::Ack(ack))) => ack,
        _ => return Err(HandshakeError::InvalidMessage("Expected ACK".into())),
    };

    // Verify network ID
    if ack.NetworkID != config.network_id {
        return Err(HandshakeError::NetworkIdMismatch);
    }

    // Build handshake info
    let observed_addrs = if !syn.ObservedUnderlay.is_empty() {
        match Multiaddr::try_from(syn.ObservedUnderlay.to_vec()) {
            Ok(addr) => vec![addr],
            Err(_) => vec![],
        }
    } else {
        vec![]
    };

    Ok(HandshakeInfo {
        peer_id: PeerId::random(), // Should be from actual peer
        full_node: ack.FullNode,
        welcome_message: ack.WelcomeMessage.into_owned(),
        observed_addrs,
    })
}

async fn handle_outbound_handshake(
    mut stream: HandshakeOutStreamSink<impl AsyncRead + AsyncWrite + Unpin>,
    config: &Config,
    remote_addr: &Multiaddr,
) -> Result<HandshakeInfo, HandshakeError> {
    // Send SYN
    let syn = Syn {
        ObservedUnderlay: Cow::Owned(remote_addr.to_vec()),
    };
    stream.send(HandshakeMessage::Syn(syn)).await?;

    // Read SYNACK
    let syn_ack = match stream.next().await {
        Some(Ok(HandshakeMessage::SynAck(syn_ack))) => syn_ack,
        _ => return Err(HandshakeError::InvalidMessage("Expected SYNACK".into())),
    };

    let ack = syn_ack
        .Ack
        .ok_or_else(|| HandshakeError::InvalidMessage("Missing ACK in SYNACK".into()))?;

    // Verify network ID
    if ack.NetworkID != config.network_id {
        return Err(HandshakeError::NetworkIdMismatch);
    }

    // Send ACK
    let response_ack = Ack {
        NetworkID: config.network_id,
        FullNode: config.full_node,
        WelcomeMessage: Cow::Borrowed(&config.welcome_message),
        ..Default::default()
    };
    stream.send(HandshakeMessage::Ack(response_ack)).await?;

    // Build handshake info
    let observed_addrs = if let Some(syn) = syn_ack.Syn {
        if !syn.ObservedUnderlay.is_empty() {
            match Multiaddr::try_from(syn.ObservedUnderlay.to_vec()) {
                Ok(addr) => vec![addr],
                Err(_) => vec![],
            }
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    Ok(HandshakeInfo {
        peer_id: PeerId::random(), // Should be from actual peer
        full_node: ack.FullNode,
        welcome_message: ack.WelcomeMessage.into_owned(),
        observed_addrs,
    })
}
