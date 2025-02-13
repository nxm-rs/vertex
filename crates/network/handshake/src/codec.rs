use alloy::primitives::B256;
use asynchronous_codec::{Decoder, Encoder};
use bytes::BytesMut;
use libp2p::Multiaddr;
use vertex_network_primitives::RemoteNodeAddressBuilder;
use vertex_network_primitives_traits::NodeAddress;

use crate::{
    proto::handshake::{Ack, BzzAddress, Syn, SynAck},
    HandshakeError,
};

#[derive(Debug)]
pub struct HandshakeSyn<const N: u64> {
    pub(crate) observed_underlay: Multiaddr,
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
    pub(crate) node_address: vertex_network_primitives::NodeAddressType<N>,
    pub(crate) network_id: u64,
    pub(crate) full_node: bool,
    pub(crate) nonce: B256,
    pub(crate) welcome_message: String,
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
    pub(crate) syn: HandshakeSyn<N>,
    pub(crate) ack: HandshakeAck<N>,
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
    pub(crate) fn new(max_packet_size: usize) -> Self {
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
    pub(crate) fn new(max_packet_size: usize) -> Self {
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
    pub(crate) fn new(max_packet_size: usize) -> Self {
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
