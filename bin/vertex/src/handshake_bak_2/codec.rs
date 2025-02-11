use alloy::primitives::{PrimitiveSignature, B256};
use asynchronous_codec::{Decoder, Encoder};
use bytes::BytesMut;
use libp2p::{core::UpgradeInfo, InboundUpgrade, Multiaddr, StreamProtocol};
use std::{array::TryFromSliceError, io};

use futures::prelude::*;

use crate::proto::handshake::{Ack, BzzAddress, Syn, SynAck};

/// The protocol name used for negotiating with multistream-select
pub(crate) const DEFAULT_PROTO_NAME: StreamProtocol =
    StreamProtocol::new("/swarm/handshake/13.0.0/handshake");
/// The default size for a varint length-delimited packet.
pub(crate) const DEFAULT_MAX_PACKET_SIZE: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error("Failed to parse observed underlay: {0}")]
    ParseUnderlay(#[from] libp2p::multiaddr::Error),
    #[error("Failed to parse nonce: {0}")]
    ParseSlice(#[from] TryFromSliceError),
    #[error("Field {0} is required")]
    MissingField(&'static str),
    #[error("Failed to parse signature: {0}")]
    SignatureError(#[from] alloy::primitives::SignatureError),
}

pub struct ProtocolConfig {
    protocol_name: Vec<StreamProtocol>,
    /// Maximum allowed size of a packet.
    max_packet_size: usize,
}

impl ProtocolConfig {
    pub fn new(protocol_name: StreamProtocol) -> Self {
        Self {
            protocol_name: vec![protocol_name],
            max_packet_size: DEFAULT_MAX_PACKET_SIZE,
        }
    }

    pub fn protocol_names(&self) -> &[StreamProtocol] {
        &self.protocol_name
    }

    /// Modifies the maximum allowed size of a single packet
    pub fn max_packet_size(&mut self, size: usize) {
        self.max_packet_size = size;
    }
}

impl UpgradeInfo for ProtocolConfig {
    type Info = StreamProtocol;
    type InfoIter = std::vec::IntoIter<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        self.protocol_name.clone().into_iter()
    }
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
    type Error = HandshakeError;

    fn try_from(value: Syn) -> Result<Self, Self::Error> {
        Ok(Self {
            observed_underlay: Multiaddr::try_from(value.ObservedUnderlay)?,
        })
    }
}

impl TryFrom<HandshakeSyn> for Syn {
    type Error = ();

    fn try_from(value: HandshakeSyn) -> Result<Self, Self::Error> {
        Ok(Syn {
            ObservedUnderlay: value.observed_underlay.to_vec(),
        })
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
    type Error = HandshakeError;

    fn try_from(value: Ack) -> Result<Self, Self::Error> {
        Ok(Self {
            node_address: value
                .Address
                .ok_or_else(|| HandshakeError::MissingField("address"))?
                .try_into()?,
            network_id: value.NetworkID,
            full_node: value.FullNode,
            nonce: B256::try_from(value.Nonce.as_slice())?,
            welcome_message: value.WelcomeMessage,
        })
    }
}

impl TryFrom<HandshakeAck> for Ack {
    type Error = ();

    fn try_from(value: HandshakeAck) -> Result<Self, Self::Error> {
        Ok(Ack {
            Address: Some(value.node_address.try_into()?),
            NetworkID: value.network_id,
            FullNode: value.full_node,
            Nonce: value.nonce.to_vec(),
            WelcomeMessage: value.welcome_message,
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

impl TryFrom<HandshakeSynAck> for SynAck {
    type Error = ();

    fn try_from(value: HandshakeSynAck) -> Result<Self, Self::Error> {
        Ok(SynAck {
            Syn: Some(value.syn.try_into()?),
            Ack: Some(value.ack.try_into()?),
        })
    }
}

#[derive(Debug)]
pub enum HandshakeMessage {
    Syn(HandshakeSyn),
    SynAck(HandshakeSynAck),
    Ack(HandshakeAck),
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

impl<A, B: TryFrom<SynAck, Error = io::Error>> Decoder for SynAckCodec<A, B> {
    type Item = B;
    type Error = io::Error;

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

impl<C> InboundUpgrade<C> for ProtocolConfig
where
    C: AsyncRead + AsyncWrite + Unpin,
{
    type Output;

    type Error = io::Error;

    type Future = future::Ready<Result<Self::Output, io::Error>>;

    fn upgrade_inbound(self, socket: C, info: Self::Info) -> Self::Future {
        todo!()
    }
}
