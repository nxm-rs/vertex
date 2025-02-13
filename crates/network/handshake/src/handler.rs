use std::{
    collections::VecDeque,
    task::{Context, Poll},
};

use alloy::primitives::B256;
use asynchronous_codec::{Framed, FramedRead};
use futures::{channel::oneshot, FutureExt, SinkExt, TryStreamExt};
use libp2p::{
    swarm::{
        handler::{ConnectionEvent, FullyNegotiatedInbound, FullyNegotiatedOutbound},
        ConnectionHandler, ConnectionHandlerEvent, SubstreamProtocol,
    },
    Multiaddr, PeerId, Stream,
};
use tracing::info;
use vertex_network_primitives::LocalNodeAddressBuilder;
use vertex_network_primitives_traits::NodeAddress;

use crate::{
    proto::handshake::{Ack, Syn, SynAck},
    AckCodec, HandshakeAck, HandshakeCommand, HandshakeConfig, HandshakeError, HandshakeEvent,
    HandshakeInfo, HandshakeProtocol, HandshakeState, HandshakeSyn, HandshakeSynAck, SynAckCodec,
    SynCodec,
};

pub struct HandshakeHandler<const N: u64> {
    config: HandshakeConfig<N>,
    state: HandshakeState,
    queued_events: VecDeque<HandshakeEvent>,
    pending_result: Option<oneshot::Receiver<Result<HandshakeInfo, HandshakeError>>>,
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

    fn do_inbound_handshake(&mut self, stream: Stream) {
        let config = self.config.clone();
        let (tx, rx) = oneshot::channel();

        let peer_id = self.peer_id.clone();
        let remote_addr = self.remote_addr.clone();
        tokio::task::spawn(async move {
            let result = handle_inbound_handshake(stream, config, peer_id, remote_addr).await;
            let _ = tx.send(result);
        });

        self.pending_result = Some(rx);
    }

    fn do_outbound_handshake(&mut self, stream: Stream) {
        let config = self.config.clone();
        let (tx, rx) = oneshot::channel();

        let peer_id = self.peer_id.clone();
        let remote_addr = self.remote_addr.clone();
        tokio::task::spawn(async move {
            let result = handle_outbound_handshake(stream, config, peer_id, remote_addr).await;
            let _ = tx.send(result);
        });

        self.pending_result = Some(rx);
    }
}

impl<const N: u64> ConnectionHandler for HandshakeHandler<N> {
    type FromBehaviour = HandshakeCommand;
    type ToBehaviour = HandshakeEvent;
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
        if let Some(rx) = &mut self.pending_result {
            match rx.poll_unpin(cx) {
                Poll::Ready(Ok(Ok(info))) => {
                    self.pending_result = None;
                    self.state = HandshakeState::Completed;
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        HandshakeEvent::Completed(info),
                    ));
                }
                Poll::Ready(Ok(Err(e))) => {
                    self.pending_result = None;
                    self.state = HandshakeState::Failed;
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        HandshakeEvent::Failed(e),
                    ));
                }
                Poll::Ready(Err(_)) => {
                    self.pending_result = None;
                    self.state = HandshakeState::Failed;
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        HandshakeEvent::Failed(HandshakeError::Protocol(
                            "Handshake future dropped".into(),
                        )),
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
                self.do_inbound_handshake(stream);
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: stream,
                ..
            }) => {
                self.do_outbound_handshake(stream);
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
) -> Result<HandshakeInfo, HandshakeError> {
    // Set up codecs
    let syn_codec = SynCodec::<Syn, HandshakeSyn<N>>::new(1024);
    let synack_codec = SynAckCodec::<SynAck, HandshakeSynAck<N>>::new(1024);
    let ack_codec = AckCodec::<Ack, HandshakeAck<N>>::new(1024);

    // Read SYN using framed read
    let mut framed = FramedRead::new(stream, syn_codec);
    let handshake_syn: HandshakeSyn<N> =
        framed.try_next().await?.ok_or(HandshakeError::InvalidSyn)?;

    // Create SYNACK
    let stream = framed.into_inner();
    let mut framed = Framed::new(stream, synack_codec);

    todo!("Finish implementing");

    // let syn_ack = HandshakeSynAck {
    //     syn: HandshakeSyn {
    //         observed_underlay: remote_addr.clone(),
    //     },
    //     ack: HandshakeAck {
    //         node_address: NodeAddress {
    //             underlay: remote_addr.clone(),
    //             signature: config.wallet.sign_message_sync(b"t")?, // Use proper signature creation
    //             overlay: B256::default(),
    //         },
    //         network_id: config.network_id,
    //         full_node: config.full_node,
    //         nonce: B256::from_slice(&config.nonce),
    //         welcome_message: config.welcome_message.clone(),
    //     },
    // };

    // framed.send(syn_ack.into()).await?;

    // // Read ACK
    // let stream = framed.into_inner();
    // let mut framed = FramedRead::new(stream, ack_codec);
    // let handshake_ack: HandshakeAck<N> =
    //     framed.try_next().await?.ok_or(HandshakeError::InvalidAck)?;

    // if handshake_ack.network_id != N {
    //     return Err(HandshakeError::NetworkIDIncompatible);
    // }

    // // Create HandshakeInfo from received data
    // Ok(HandshakeInfo {
    //     peer_id: PeerId::random(),      // Should come from actual peer ID
    //     address: FixedBytes::default(), // Should come from actual address
    //     full_node: handshake_ack.full_node,
    //     welcome_message: handshake_ack.welcome_message,
    //     observed_underlay: vec![remote_addr],
    // })
}

async fn handle_outbound_handshake<const N: u64>(
    stream: Stream,
    config: HandshakeConfig<N>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
) -> Result<HandshakeInfo, HandshakeError> {
    info!("Remote address: {:?}", remote_addr);

    const MAINNET: u64 = 1;

    let syn_codec = SynCodec::<Syn, HandshakeSyn<N>>::new(1024);
    let synack_codec = SynAckCodec::<SynAck, HandshakeSynAck<N>>::new(1024);
    let ack_codec = AckCodec::<Ack, HandshakeAck<N>>::new(1024);

    let mut framed = Framed::new(stream, syn_codec);
    framed
        .send(
            HandshakeSyn::<N> {
                observed_underlay: remote_addr.clone(),
            }
            .into(),
        )
        .await?;

    // Read SYNACK
    let stream = framed.into_inner();
    let mut framed = FramedRead::new(stream, synack_codec);
    let syn_ack: HandshakeSynAck<N> = framed.try_next().await?.ok_or(HandshakeError::InvalidSyn)?;

    if syn_ack.ack.network_id != N {
        return Err(HandshakeError::NetworkIDIncompatible);
    }

    // Create longer-lived buffer
    let local_address = LocalNodeAddressBuilder::<MAINNET, _>::new()
        .with_nonce(B256::ZERO)
        .with_underlay(syn_ack.syn.observed_underlay.clone())
        .with_signer(config.wallet.clone())?
        .build()?;

    // Send ACK
    let stream = framed.into_inner();
    let mut framed = Framed::new(stream, ack_codec);

    framed
        .send(
            HandshakeAck {
                node_address: local_address.clone(),
                network_id: N,
                full_node: config.full_node,
                nonce: local_address.nonce().clone(),
                welcome_message: config.welcome_message.clone(),
            }
            .into(),
        )
        .await?;

    framed.close().await?;

    // Create HandshakeInfo from received data
    Ok(HandshakeInfo {
        peer_id,
        address: syn_ack.ack.node_address.overlay_address().clone(),
        full_node: syn_ack.ack.full_node,
        welcome_message: syn_ack.ack.welcome_message,
        observed_underlay: vec![syn_ack.ack.node_address.underlay_address().clone()],
    })
}
