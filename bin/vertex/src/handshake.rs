use std::{
    array::TryFromSliceError,
    borrow::Cow,
    collections::{HashMap, VecDeque},
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use alloy::{
    hex::ToHexExt,
    primitives::{FixedBytes, PrimitiveSignature, Signature, B256},
    signers::{local::PrivateKeySigner, SignerSync},
};
use asynchronous_codec::{Decoder, Encoder, Framed, FramedRead};
use bytes::BytesMut;
use futures::{AsyncReadExt, AsyncWriteExt, SinkExt, StreamExt, TryStreamExt};
use libp2p::{
    core::{
        transport::PortUse,
        upgrade::{InboundUpgrade, OutboundUpgrade, UpgradeInfo},
        Endpoint,
    },
    swarm::{
        ConnectionDenied, ConnectionHandler, ConnectionId, FromSwarm, NetworkBehaviour,
        NotifyHandler, SubstreamProtocol, ToSwarm,
    },
    Multiaddr, PeerId, Stream, StreamProtocol,
};
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Reader, Writer};
use tracing::info;

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
}

#[derive(Debug, Clone)]
pub struct HandshakeConfig {
    pub network_id: u64,
    pub protocol_version: String,
    pub full_node: bool,
    pub nonce: Vec<u8>,
    pub welcome_message: String,
    pub validate_overlay: bool,
    pub wallet: PrivateKeySigner,
}

impl Default for HandshakeConfig {
    fn default() -> Self {
        Self {
            network_id: 1,
            protocol_version: PROTOCOL_VERSION.to_string(),
            full_node: true,
            nonce: vec![0; 32],
            welcome_message: String::new(),
            validate_overlay: true,
            wallet: PrivateKeySigner::random(),
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
pub enum HandshakeHandlerEvent {
    StartHandshake,
    HandshakeCompleted(HandshakeInfo),
    HandshakeFailed(HandshakeError),
}

#[derive(Debug)]
pub enum HandshakeEvent {
    Completed {
        peer: PeerId,
        connection: ConnectionId,
        info: HandshakeInfo,
    },
    Failed {
        peer: PeerId,
        connection: ConnectionId,
        error: HandshakeError,
    },
}

pub struct HandshakeBehaviour {
    config: HandshakeConfig,
    handshaked_peers: HashMap<PeerId, PeerState>,
    events: VecDeque<ToSwarm<HandshakeEvent, HandshakeHandlerEvent>>,
}

impl HandshakeBehaviour {
    pub fn new(config: HandshakeConfig) -> Self {
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

impl NetworkBehaviour for HandshakeBehaviour {
    type ConnectionHandler = HandshakeHandler;
    type ToSwarm = HandshakeEvent;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(HandshakeHandler::new(
            self.config.clone(),
            remote_addr.clone(),
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(HandshakeHandler::new(self.config.clone(), addr.clone()))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(connection) => {
                self.events.push_back(ToSwarm::NotifyHandler {
                    peer_id: connection.peer_id,
                    handler: NotifyHandler::One(connection.connection_id),
                    event: HandshakeHandlerEvent::StartHandshake,
                });
            }
            FromSwarm::ConnectionClosed(connection) => {
                if let Some(state) = self.handshaked_peers.get_mut(&connection.peer_id) {
                    state.connections.retain(|c| *c != connection.connection_id);
                    if state.connections.is_empty() {
                        self.handshaked_peers.remove(&connection.peer_id);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: HandshakeHandlerEvent,
    ) {
        match event {
            HandshakeHandlerEvent::HandshakeCompleted(info) => {
                self.handshaked_peers
                    .entry(peer_id)
                    .and_modify(|state| {
                        if !state.connections.contains(&connection_id) {
                            state.connections.push(connection_id);
                        }
                    })
                    .or_insert_with(|| PeerState {
                        info: info.clone(),
                        connections: vec![connection_id],
                    });

                self.events
                    .push_back(ToSwarm::GenerateEvent(HandshakeEvent::Completed {
                        peer: peer_id,
                        connection: connection_id,
                        info,
                    }));
            }
            HandshakeHandlerEvent::HandshakeFailed(error) => {
                self.events
                    .push_back(ToSwarm::GenerateEvent(HandshakeEvent::Failed {
                        peer: peer_id,
                        connection: connection_id,
                        error,
                    }));
            }
            HandshakeHandlerEvent::StartHandshake => {}
        }
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, HandshakeHandlerEvent>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }
        Poll::Pending
    }
}

#[derive(Debug, Clone)]
pub struct HandshakeProtocol {
    config: HandshakeConfig,
    remote_addr: Multiaddr,
}

impl UpgradeInfo for HandshakeProtocol {
    type Info = StreamProtocol;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(StreamProtocol::new("/swarm/handshake/13.0.0/handshake"))
    }
}

impl InboundUpgrade<Stream> for HandshakeProtocol {
    type Output = HandshakeInfo;
    type Error = HandshakeError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_inbound(self, stream: Stream, _: Self::Info) -> Self::Future {
        Box::pin(handle_inbound_handshake(
            stream,
            self.config,
            self.remote_addr.clone(),
        ))
    }
}

impl OutboundUpgrade<Stream> for HandshakeProtocol {
    type Output = HandshakeInfo;
    type Error = HandshakeError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_outbound(self, stream: Stream, _: Self::Info) -> Self::Future {
        Box::pin(handle_outbound_handshake(
            stream,
            self.config,
            self.remote_addr,
        ))
    }
}

pub struct HandshakeHandler {
    config: HandshakeConfig,
    pending_handshake: Option<()>,
    state: HandshakeState,
    queued_events: VecDeque<HandshakeHandlerEvent>,
    remote_addr: Multiaddr,
}

#[derive(Debug, Clone, PartialEq)]
enum HandshakeState {
    Idle,
    Handshaking,
    Completed,
    Failed,
}

impl HandshakeHandler {
    pub fn new(config: HandshakeConfig, remote_addr: Multiaddr) -> Self {
        Self {
            config,
            pending_handshake: None,
            state: HandshakeState::Idle,
            queued_events: VecDeque::new(),
            remote_addr,
        }
    }
}

impl ConnectionHandler for HandshakeHandler {
    type FromBehaviour = HandshakeHandlerEvent;
    type ToBehaviour = HandshakeHandlerEvent;
    type InboundProtocol = HandshakeProtocol;
    type OutboundProtocol = HandshakeProtocol;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol> {
        SubstreamProtocol::new(
            HandshakeProtocol {
                config: self.config.clone(),
                remote_addr: self.remote_addr.clone(),
            },
            (),
        )
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            HandshakeHandlerEvent::StartHandshake => {
                if self.pending_handshake.is_none() {
                    self.state = HandshakeState::Idle;
                }
            }
            _ => {}
        }
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<
        libp2p::swarm::ConnectionHandlerEvent<
            Self::OutboundProtocol,
            Self::OutboundOpenInfo,
            Self::ToBehaviour,
        >,
    > {
        if let Some(event) = self.queued_events.pop_front() {
            return Poll::Ready(libp2p::swarm::ConnectionHandlerEvent::NotifyBehaviour(
                event,
            ));
        }

        if self.pending_handshake.is_none() && matches!(self.state, HandshakeState::Idle) {
            self.state = HandshakeState::Handshaking;
            self.pending_handshake = Some(());
            return Poll::Ready(
                libp2p::swarm::ConnectionHandlerEvent::OutboundSubstreamRequest {
                    protocol: SubstreamProtocol::new(
                        HandshakeProtocol {
                            config: self.config.clone(),
                            remote_addr: self.remote_addr.clone(),
                        },
                        (),
                    ),
                },
            );
        }

        Poll::Pending
    }

    fn on_connection_event(
        &mut self,
        event: libp2p::swarm::handler::ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        use libp2p::swarm::handler::ConnectionEvent::*;

        match event {
            FullyNegotiatedInbound(stream) => {
                self.state = HandshakeState::Completed;
                self.pending_handshake = None;
                // self.queued_events
                //     .push_back(HandshakeHandlerEvent::HandshakeCompleted(stream.0));
            }
            FullyNegotiatedOutbound(stream) => {
                self.state = HandshakeState::Completed;
                self.pending_handshake = None;
                // self.queued_events
                //     .push_back(HandshakeHandlerEvent::HandshakeCompleted(stream.0));
            }
            DialUpgradeError(e) => {
                self.state = HandshakeState::Failed;
                self.pending_handshake = None;
                self.queued_events
                    .push_back(HandshakeHandlerEvent::HandshakeFailed(
                        HandshakeError::Protocol(e.error.to_string()),
                    ));
            }
            ListenUpgradeError(e) => {
                self.state = HandshakeState::Failed;
                self.pending_handshake = None;
                self.queued_events
                    .push_back(HandshakeHandlerEvent::HandshakeFailed(
                        HandshakeError::Protocol(e.error.to_string()),
                    ));
            }
            _ => {}
        }
    }
}

async fn handle_inbound_handshake(
    stream: Stream,
    config: HandshakeConfig,
    remote_addr: Multiaddr,
) -> Result<HandshakeInfo, HandshakeError> {
    // Set up codecs
    let syn_codec = SynCodec::<Syn, HandshakeSyn>::new(1024);
    let synack_codec = SynAckCodec::<SynAck, HandshakeSynAck>::new(1024);
    let ack_codec = AckCodec::<Ack, HandshakeAck>::new(1024);

    // Read SYN using framed read
    let mut framed = FramedRead::new(stream, syn_codec);
    let handshake_syn: HandshakeSyn = framed.try_next().await?.ok_or(HandshakeError::InvalidSyn)?;

    // Create SYNACK
    let stream = framed.into_inner();
    let mut framed = Framed::new(stream, synack_codec);

    let syn_ack = HandshakeSynAck {
        syn: HandshakeSyn {
            observed_underlay: remote_addr.clone(),
        },
        ack: HandshakeAck {
            node_address: NodeAddress {
                underlay: remote_addr.clone(),
                signature: config.wallet.sign_message_sync(b"t")?, // Use proper signature creation
                overlay: B256::default(),
            },
            network_id: config.network_id,
            full_node: config.full_node,
            nonce: B256::from_slice(&config.nonce),
            welcome_message: config.welcome_message.clone(),
        },
    };

    framed.send(syn_ack.into()).await?;

    // Read ACK
    let stream = framed.into_inner();
    let mut framed = FramedRead::new(stream, ack_codec);
    let handshake_ack: HandshakeAck = framed.try_next().await?.ok_or(HandshakeError::InvalidAck)?;

    if handshake_ack.network_id != config.network_id {
        return Err(HandshakeError::NetworkIDIncompatible);
    }

    // Create HandshakeInfo from received data
    Ok(HandshakeInfo {
        peer_id: PeerId::random(),      // Should come from actual peer ID
        address: FixedBytes::default(), // Should come from actual address
        full_node: handshake_ack.full_node,
        welcome_message: handshake_ack.welcome_message,
        observed_underlay: vec![remote_addr],
    })
}

async fn handle_outbound_handshake(
    mut stream: Stream,
    config: HandshakeConfig,
    remote_addr: Multiaddr,
) -> Result<HandshakeInfo, HandshakeError> {
    info!("Remote address: {:?}", remote_addr);

    // Create longer-lived buffer

    let syn_codec = SynCodec::<Syn, HandshakeSyn>::new(1024);
    let synack_codec = SynAckCodec::<SynAck, HandshakeSynAck>::new(1024);
    let ack_codec = AckCodec::<Ack, HandshakeAck>::new(1024);

    let mut framed = Framed::new(stream, syn_codec);
    framed
        .send(
            HandshakeSyn {
                observed_underlay: remote_addr.clone(),
            }
            .into(),
        )
        .await
        .unwrap();

    info!("SYN sent");

    // Read SYNACK
    let stream = framed.into_inner();
    let mut framed = FramedRead::new(stream, synack_codec);
    let syn_ack: HandshakeSynAck = framed.try_next().await?.ok_or(HandshakeError::InvalidSyn)?;

    info!("SYNACK received: {:?}", syn_ack);

    if syn_ack.ack.network_id != config.network_id {
        return Err(HandshakeError::NetworkIDIncompatible);
    }

    // Send ACK
    let stream = framed.into_inner();
    let mut framed = Framed::new(stream, ack_codec);
    framed
        .send(
            HandshakeAck {
                node_address: NodeAddress {
                    underlay: remote_addr.clone(),
                    signature: config.wallet.sign_message_sync(&remote_addr.to_vec())?,
                    overlay: B256::default(),
                },
                network_id: config.network_id,
                full_node: config.full_node,
                nonce: B256::default(),
                welcome_message: config.welcome_message.clone(),
            }
            .into(),
        )
        .await
        .unwrap();

    // Create HandshakeInfo from received data
    Ok(HandshakeInfo {
        peer_id: PeerId::random(),      // Should come from actual peer ID
        address: FixedBytes::default(), // Should come from actual address
        full_node: false,
        welcome_message: "str".to_string(),
        observed_underlay: vec![remote_addr],
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAddress {
    underlay: Multiaddr,
    signature: PrimitiveSignature,
    overlay: B256,
}

impl TryFrom<BzzAddress> for NodeAddress {
    type Error = HandshakeError;

    fn try_from(value: BzzAddress) -> Result<Self, Self::Error> {
        Ok(Self {
            underlay: Multiaddr::try_from(value.Underlay)?,
            signature: PrimitiveSignature::try_from(value.Signature.as_slice())?,
            overlay: B256::try_from(value.Overlay.as_slice())?,
        })
    }
}

impl TryFrom<NodeAddress> for BzzAddress {
    type Error = ();

    fn try_from(value: NodeAddress) -> Result<Self, Self::Error> {
        Ok(BzzAddress {
            Underlay: value.underlay.to_vec(),
            Signature: value.signature.as_bytes().to_vec(),
            Overlay: value.overlay.to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeSyn {
    observed_underlay: Multiaddr,
}

impl TryFrom<Syn> for HandshakeSyn {
    type Error = io::Error;

    fn try_from(value: Syn) -> Result<Self, Self::Error> {
        Ok(Self {
            observed_underlay: Multiaddr::try_from(value.ObservedUnderlay)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        })
    }
}

impl Into<Syn> for HandshakeSyn {
    fn into(self) -> Syn {
        Syn {
            ObservedUnderlay: self.observed_underlay.to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeAck {
    node_address: NodeAddress,
    network_id: u64,
    full_node: bool,
    nonce: B256,
    welcome_message: String,
}

impl TryFrom<Ack> for HandshakeAck {
    type Error = io::Error;

    fn try_from(value: Ack) -> Result<Self, Self::Error> {
        Ok(Self {
            node_address: value
                .Address
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Address field missing"))?
                .try_into()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            network_id: value.NetworkID,
            full_node: value.FullNode,
            nonce: B256::try_from(value.Nonce.as_slice())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            welcome_message: value.WelcomeMessage,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeSynAck {
    syn: HandshakeSyn,
    ack: HandshakeAck,
}

impl TryFrom<SynAck> for HandshakeSynAck {
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

impl From<HandshakeSynAck> for SynAck {
    fn from(value: HandshakeSynAck) -> Self {
        SynAck {
            Syn: Some(value.syn.into()),
            Ack: Some(value.ack.into()),
        }
    }
}

impl From<HandshakeAck> for Ack {
    fn from(value: HandshakeAck) -> Self {
        Ack {
            Address: Some(
                value
                    .node_address
                    .try_into()
                    .expect("Failed to convert NodeAddress"),
            ),
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
    type Error = io::Error;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        Ok(self.codec.encode(item.into(), dst)?)
    }
}

impl<A, B: TryFrom<Syn, Error = io::Error>> Decoder for SynCodec<A, B> {
    type Item = B;
    type Error = io::Error;

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
    type Error = io::Error;

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
    type Error = io::Error;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        Ok(self.codec.encode(item.into(), dst)?)
    }
}

impl<A, B: TryFrom<Ack, Error = io::Error>> Decoder for AckCodec<A, B> {
    type Item = B;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.codec.decode(src)?.map(B::try_from).transpose()
    }
}
