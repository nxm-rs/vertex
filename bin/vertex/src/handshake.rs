use std::{
    array::TryFromSliceError,
    collections::{HashMap, VecDeque},
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use alloy::{
    primitives::{FixedBytes, B256},
    signers::local::PrivateKeySigner,
};
use asynchronous_codec::{Decoder, Encoder, Framed, FramedRead};
use bytes::BytesMut;
use futures::{channel::oneshot, future::BoxFuture, FutureExt, SinkExt, TryStreamExt};
use libp2p::{
    core::{
        transport::PortUse,
        upgrade::{InboundUpgrade, OutboundUpgrade, UpgradeInfo},
        Endpoint,
    },
    swarm::{
        handler::{ConnectionEvent, FullyNegotiatedInbound, FullyNegotiatedOutbound},
        ConnectionDenied, ConnectionHandler, ConnectionHandlerEvent, ConnectionId, FromSwarm,
        NetworkBehaviour, NotifyHandler, SubstreamProtocol, THandlerInEvent, ToSwarm,
    },
    Multiaddr, PeerId, Stream,
};
use tracing::info;
use vertex_network_primitives::{LocalNodeAddressBuilder, RemoteNodeAddressBuilder};
use vertex_network_primitives_traits::NodeAddress;

use crate::proto::handshake::{Ack, BzzAddress, Syn, SynAck};

// Include protobuf generated code
// Constants
const PROTOCOL_VERSION: &str = "13.0.0";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_WELCOME_MESSAGE_LENGTH: usize = 140;

#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error("Network ID incompatible")]
    NetworkIDIncompatible,
    #[error("Invalid ACK")]
    InvalidAck,
    #[error("Invalid SYN")]
    InvalidSyn,
    #[error("Welcome message too long")]
    WelcomeMessageTooLong,
    #[error("Picker rejection")]
    PickerRejection,
    #[error("Timeout")]
    Timeout,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("Quick protobuf error: {0}")]
    QuickProtobuf(#[from] quick_protobuf::Error),
    #[error("Unsigned varint error: {0}")]
    UnsignedVarint(#[from] unsigned_varint::decode::Error),
    #[error("Failed to parse observed underlay: {0}")]
    ParseUnderlay(#[from] libp2p::multiaddr::Error),
    #[error("Failed to parse nonce: {0}")]
    ParseSlice(#[from] TryFromSliceError),
    #[error("Field {0} is required")]
    MissingField(&'static str),
    #[error("Failed to parse signature: {0}")]
    SignatureError(#[from] alloy::primitives::SignatureError),
    #[error("Alloy signer error: {0}")]
    SignerError(#[from] alloy::signers::Error),
    #[error("NodeAddress conversion error: {0}")]
    NodeAddressConversion(#[from] vertex_network_primitives_traits::NodeAddressError),
}

#[derive(Debug, Clone)]
pub struct HandshakeConfig<const N: u64> {
    pub protocol_version: String,
    pub full_node: bool,
    pub nonce: Vec<u8>,
    pub welcome_message: String,
    pub validate_overlay: bool,
    pub wallet: Arc<PrivateKeySigner>,
}

impl<const N: u64> Default for HandshakeConfig<N> {
    fn default() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            full_node: true,
            nonce: vec![0; 32],
            welcome_message: "Vertex into the Swarm".to_string(),
            validate_overlay: true,
            wallet: Arc::new(PrivateKeySigner::random()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HandshakeInfo {
    pub peer_id: PeerId,
    pub address: FixedBytes<32>,
    pub full_node: bool,
    pub welcome_message: String,
    pub observed_underlay: Vec<Multiaddr>,
}

#[derive(Debug, Clone)]
pub struct PeerState {
    pub info: HandshakeInfo,
    pub connections: Vec<ConnectionId>,
}

#[derive(Debug)]
pub enum HandshakeEvent {
    Completed(HandshakeInfo),
    Failed(HandshakeError),
}

pub struct HandshakeBehaviour<const N: u64> {
    config: HandshakeConfig<N>,
    handshaked_peers: HashMap<PeerId, PeerState>,
    pub events: VecDeque<ToSwarm<HandshakeEvent, HandshakeCommand>>,
}

impl<const N: u64> HandshakeBehaviour<N> {
    pub fn new(config: HandshakeConfig<N>) -> Self {
        Self {
            config,
            handshaked_peers: HashMap::new(),
            events: VecDeque::new(),
        }
    }

    pub fn peer_info(&self, peer: &PeerId) -> Option<&HandshakeInfo> {
        self.handshaked_peers.get(peer).map(|state| &state.info)
    }

    pub fn handshaked_peers(&self) -> impl Iterator<Item = (&PeerId, &HandshakeInfo)> {
        self.handshaked_peers
            .iter()
            .map(|(id, state)| (id, &state.info))
    }

    pub fn is_peer_handshaked(&self, peer: &PeerId) -> bool {
        self.handshaked_peers.contains_key(peer)
    }
}

impl<const N: u64> NetworkBehaviour for HandshakeBehaviour<N> {
    type ConnectionHandler = HandshakeHandler<N>;
    type ToSwarm = HandshakeEvent;

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(connection) => {
                // Start handshake when connection is established
                self.events.push_back(ToSwarm::NotifyHandler {
                    peer_id: connection.peer_id,
                    handler: NotifyHandler::One(connection.connection_id),
                    event: HandshakeCommand::StartHandshake,
                });
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        event: HandshakeEvent,
    ) {
        match event {
            HandshakeEvent::Completed(info) => {
                // Store peer info on successful handshake
                self.handshaked_peers.insert(
                    peer_id,
                    PeerState {
                        info: info.clone(),
                        connections: vec![connection],
                    },
                );
                self.events
                    .push_back(ToSwarm::GenerateEvent(HandshakeEvent::Completed(info)));
            }
            HandshakeEvent::Failed(error) => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(HandshakeEvent::Failed(error)));
            }
        }
    }

    fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }
        Poll::Pending
    }

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<libp2p::swarm::THandler<Self>, ConnectionDenied> {
        Ok(HandshakeHandler::new(
            self.config.clone(),
            peer,
            remote_addr.clone(),
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
        port_use: PortUse,
    ) -> Result<libp2p::swarm::THandler<Self>, ConnectionDenied> {
        Ok(HandshakeHandler::new(
            self.config.clone(),
            peer,
            addr.clone(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct HandshakeProtocol<const N: u64> {
    config: HandshakeConfig<N>,
}

impl<const N: u64> UpgradeInfo for HandshakeProtocol<N> {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once("/swarm/handshake/13.0.0/handshake")
    }
}

impl<const N: u64> InboundUpgrade<Stream> for HandshakeProtocol<N> {
    type Output = Stream;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        // Just return the negotiated stream
        Box::pin(futures::future::ok(socket))
    }
}

impl<const N: u64> OutboundUpgrade<Stream> for HandshakeProtocol<N> {
    type Output = Stream;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        // Just return the negotiated stream
        Box::pin(futures::future::ok(socket))
    }
}

pub struct HandshakeHandler<const N: u64> {
    config: HandshakeConfig<N>,
    state: HandshakeState,
    queued_events: VecDeque<HandshakeEvent>,
    pending_result: Option<oneshot::Receiver<Result<HandshakeInfo, HandshakeError>>>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
}

#[derive(Debug)]
enum HandshakeState {
    Idle,
    Start,
    Handshaking,
    Completed,
    Failed,
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

#[derive(Debug)]
pub enum HandshakeCommand {
    StartHandshake,
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

#[derive(Debug)]
pub struct HandshakeSyn<const N: u64> {
    observed_underlay: Multiaddr,
}

impl<const N: u64> TryFrom<Syn> for HandshakeSyn<N> {
    type Error = HandshakeError;

    fn try_from(value: Syn) -> Result<Self, Self::Error> {
        Ok(Self {
            observed_underlay: Multiaddr::try_from(value.ObservedUnderlay)?,
        })
    }
}

impl<const N: u64> Into<Syn> for HandshakeSyn<N> {
    fn into(self) -> Syn {
        Syn {
            ObservedUnderlay: self.observed_underlay.to_vec(),
        }
    }
}

#[derive(Debug)]
pub struct HandshakeAck<const N: u64> {
    node_address: vertex_network_primitives::NodeAddressType<N>,
    network_id: u64,
    full_node: bool,
    nonce: B256,
    welcome_message: String,
}

impl<const N: u64> TryFrom<Ack> for HandshakeAck<N> {
    type Error = HandshakeError;

    fn try_from(value: Ack) -> Result<Self, Self::Error> {
        let protobuf_address = value
            .Address
            .as_ref()
            .ok_or_else(|| HandshakeError::MissingField("address"))?;
        let remote_address = RemoteNodeAddressBuilder::new()
            .with_nonce(value.Nonce.as_slice().try_into()?)
            .with_underlay(Multiaddr::try_from(protobuf_address.Underlay.clone())?)
            .with_identity(
                protobuf_address.Overlay.as_slice().try_into()?,
                protobuf_address.Signature.as_slice().try_into()?,
            )?
            .build()?;
        Ok(Self {
            node_address: remote_address,
            network_id: value.NetworkID,
            full_node: value.FullNode,
            nonce: B256::try_from(value.Nonce.as_slice())?,
            welcome_message: value.WelcomeMessage,
        })
    }
}

#[derive(Debug)]
pub struct HandshakeSynAck<const N: u64> {
    syn: HandshakeSyn<N>,
    ack: HandshakeAck<N>,
}

impl<const N: u64> TryFrom<SynAck> for HandshakeSynAck<N> {
    type Error = HandshakeError;

    fn try_from(value: SynAck) -> Result<Self, Self::Error> {
        Ok(Self {
            syn: value
                .Syn
                .ok_or_else(|| HandshakeError::MissingField("syn"))?
                .try_into()?,
            ack: value
                .Ack
                .ok_or_else(|| HandshakeError::MissingField("ack"))?
                .try_into()?,
        })
    }
}

impl<const N: u64> From<HandshakeSynAck<N>> for SynAck {
    fn from(value: HandshakeSynAck<N>) -> Self {
        SynAck {
            Syn: Some(value.syn.into()),
            Ack: Some(value.ack.into()),
        }
    }
}

impl<const N: u64> From<HandshakeAck<N>> for Ack {
    fn from(value: HandshakeAck<N>) -> Self {
        Ack {
            Address: Some(BzzAddress {
                Underlay: value.node_address.underlay_address().to_vec(),
                Signature: value.node_address.signature().unwrap().as_bytes().to_vec(),
                Overlay: value.node_address.overlay_address().to_vec(),
            }),
            NetworkID: value.network_id,
            FullNode: value.full_node,
            Nonce: value.nonce.to_vec(),
            WelcomeMessage: value.welcome_message,
        }
    }
}

// Add From implementation for the codec error
impl From<quick_protobuf_codec::Error> for HandshakeError {
    fn from(error: quick_protobuf_codec::Error) -> Self {
        HandshakeError::Protocol(error.to_string())
    }
}

/// Codec for Handshake inbound and outbound message framing
pub struct SynCodec<A, B> {
    codec: quick_protobuf_codec::Codec<Syn>,
    __phantom: std::marker::PhantomData<(A, B)>,
}
impl<A, B> SynCodec<A, B> {
    fn new(max_packet_size: usize) -> Self {
        Self {
            codec: quick_protobuf_codec::Codec::new(max_packet_size),
            __phantom: std::marker::PhantomData,
        }
    }
}

impl<A: Into<Syn>, B> Encoder for SynCodec<A, B> {
    type Item<'a> = A;
    type Error = HandshakeError;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        Ok(self.codec.encode(item.into(), dst)?)
    }
}

impl<A, B: TryFrom<Syn, Error = HandshakeError>> Decoder for SynCodec<A, B> {
    type Item = B;
    type Error = HandshakeError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.codec.decode(src)?.map(B::try_from).transpose()
    }
}

/// Codec for Handshake inbound and outbound message framing
pub struct SynAckCodec<A, B> {
    codec: quick_protobuf_codec::Codec<SynAck>,
    __phantom: std::marker::PhantomData<(A, B)>,
}

impl<A, B> SynAckCodec<A, B> {
    fn new(max_packet_size: usize) -> Self {
        Self {
            codec: quick_protobuf_codec::Codec::new(max_packet_size),
            __phantom: std::marker::PhantomData,
        }
    }
}

impl<A: Into<SynAck>, B> Encoder for SynAckCodec<A, B> {
    type Item<'a> = A;
    type Error = HandshakeError;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        Ok(self.codec.encode(item.into(), dst)?)
    }
}

impl<A, B: TryFrom<SynAck, Error = HandshakeError>> Decoder for SynAckCodec<A, B> {
    type Item = B;
    type Error = HandshakeError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.codec.decode(src)?.map(B::try_from).transpose()
    }
}

/// Codec for Handshake inbound and outbound message framing
/// This codec is used for the final ACK message
pub struct AckCodec<A, B> {
    codec: quick_protobuf_codec::Codec<Ack>,
    __phantom: std::marker::PhantomData<(A, B)>,
}

impl<A, B> AckCodec<A, B> {
    fn new(max_packet_size: usize) -> Self {
        Self {
            codec: quick_protobuf_codec::Codec::new(max_packet_size),
            __phantom: std::marker::PhantomData,
        }
    }
}

impl<A: Into<Ack>, B> Encoder for AckCodec<A, B> {
    type Item<'a> = A;
    type Error = HandshakeError;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        Ok(self.codec.encode(item.into(), dst)?)
    }
}

impl<A, B: TryFrom<Ack, Error = HandshakeError>> Decoder for AckCodec<A, B> {
    type Item = B;
    type Error = HandshakeError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.codec.decode(src)?.map(B::try_from).transpose()
    }
}
