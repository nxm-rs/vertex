use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use alloy::primitives::B256;
use asynchronous_codec::{Framed, FramedRead};
use futures::{channel::oneshot, AsyncWriteExt, FutureExt, SinkExt, StreamExt};
use libp2p::{
    swarm::{
        handler::{ConnectionEvent, FullyNegotiatedInbound, FullyNegotiatedOutbound},
        ConnectionHandler, ConnectionHandlerEvent, SubstreamProtocol,
    },
    Multiaddr, PeerId, Stream,
};
use tracing::{debug, info};
use vertex_network_primitives::NodeAddress;

use crate::{
    codec::*, HandshakeCommand, HandshakeConfig, HandshakeError, HandshakeEvent, HandshakeInfo,
    HandshakeProtocol, HandshakeState, HANDSHAKE_TIMEOUT,
};

pub struct HandshakeHandler<const N: u64> {
    config: HandshakeConfig<N>,
    state: HandshakeState,
    queued_events: VecDeque<HandshakeEvent<N>>,
    pending_result:
        Option<Pin<Box<dyn Future<Output = Result<HandshakeInfo<N>, HandshakeError>> + Send>>>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
}

impl<const N: u64> HandshakeHandler<N> {
    pub fn new(config: HandshakeConfig<N>, peer_id: PeerId, remote_addr: Multiaddr) -> Self {
        Self {
            config,
            state: HandshakeState::Idle,
            queued_events: VecDeque::new(),
            pending_result: None,
            peer_id,
            remote_addr,
        }
    }

    fn handle_handshake(&mut self, stream: Stream, is_inbound: bool) {
        let future: Pin<Box<dyn Future<Output = Result<HandshakeInfo<N>, HandshakeError>> + Send>> =
            if is_inbound {
                Box::pin(handle_inbound_handshake(
                    stream,
                    self.config.clone(),
                    self.peer_id,
                    self.remote_addr.clone(),
                ))
            } else {
                Box::pin(handle_outbound_handshake(
                    stream,
                    self.config.clone(),
                    self.peer_id,
                    self.remote_addr.clone(),
                ))
            };

        self.pending_result = Some(future);
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
                protocol: stream,
                ..
            }) => {
                self.handle_handshake(stream, true);
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: stream,
                ..
            }) => {
                self.handle_handshake(stream, false);
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

async fn handle_inbound_handshake<const N: u64>(
    stream: Stream,
    config: HandshakeConfig<N>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
) -> Result<HandshakeInfo<N>, HandshakeError> {
    // Set up codecs
    let syn_codec = SynCodec::<N>::new(1024);
    let synack_codec = SynAckCodec::<N>::new(1024);
    let ack_codec = AckCodec::<N>::new(1024);

    // Read SYN using framed read
    let mut framed = FramedRead::new(stream, syn_codec);
    debug!("Attempting to read SYN");
    let syn = match framed.next().await {
        Some(Ok(syn)) => syn,
        Some(Err(e)) => return Err(e.into()),
        None => {
            debug!("Connection closed while reading SYN");
            return Err(HandshakeError::Stream(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed",
            )));
        }
    };
    debug!("Received SYN: {:?}", syn);

    // Create local address
    let local_address: NodeAddress<N> = NodeAddress::builder()
        .with_nonce(config.nonce)
        .with_underlay(syn.observed_underlay.clone())
        .with_signer(config.wallet.clone())
        .map_err(|e| HandshakeError::Codec(e.into()))?
        .build();

    // Send SYNACK
    let synack = SynAck {
        syn,
        ack: Ack {
            node_address: local_address,
            full_node: config.full_node,
            welcome_message: config.welcome_message.clone(),
        },
    };

    let mut framed = Framed::new(framed.into_inner(), synack_codec);
    framed.send(synack).await?;

    // Read ACK
    let mut framed = FramedRead::new(framed.into_inner(), ack_codec);
    debug!("Attempting to read ACK");
    let ack = match framed.next().await {
        Some(Ok(ack)) => ack,
        Some(Err(e)) => return Err(e.into()),
        None => {
            debug!("Connection closed before ACK was received");
            return Err(HandshakeError::Stream(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed",
            )));
        }
    };
    debug!("Received ACK: {:?}", ack);

    framed.close().await;

    Ok(HandshakeInfo {
        peer_id,
        address: ack.node_address,
        full_node: ack.full_node,
        welcome_message: ack.welcome_message,
    })
}

async fn handle_outbound_handshake<const N: u64>(
    stream: Stream,
    config: HandshakeConfig<N>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
) -> Result<HandshakeInfo<N>, HandshakeError> {
    info!("Remote address: {:?}", remote_addr);

    let syn_codec = SynCodec::<N>::new(1024);
    let synack_codec = SynAckCodec::<N>::new(1024);
    let ack_codec = AckCodec::<N>::new(1024);

    let mut framed = Framed::new(stream, syn_codec);
    framed
        .send(
            Syn {
                observed_underlay: remote_addr.clone(),
            }
            .into(),
        )
        .await?;

    // Read SYNACK
    let mut framed = FramedRead::new(framed.into_inner(), synack_codec);
    debug!("Attempting to read SYNACK");
    let syn_ack = match framed.next().await {
        Some(Ok(syn)) => syn,
        Some(Err(e)) => return Err(e.into()),
        None => {
            debug!("Connection closed before SYNACK was received");
            return Err(HandshakeError::Stream(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed",
            )));
        }
    };
    debug!("Received SYNACK: {:?}", syn_ack);

    let local_address: NodeAddress<N> = NodeAddress::builder()
        .with_nonce(config.nonce)
        .with_underlay(syn_ack.syn.observed_underlay.clone())
        .with_signer(config.wallet.clone())
        .map_err(|e| HandshakeError::Codec(e.into()))?
        .build();

    // Send ACK
    let mut framed = Framed::new(framed.into_inner(), ack_codec);

    framed
        .send(Ack {
            node_address: local_address,
            full_node: config.full_node,
            welcome_message: config.welcome_message.clone(),
        })
        .await?;

    framed.close().await?;

    // Create HandshakeInfo from received data
    Ok(HandshakeInfo {
        peer_id,
        address: syn_ack.ack.node_address,
        full_node: syn_ack.ack.full_node,
        welcome_message: syn_ack.ack.welcome_message,
    })
}
