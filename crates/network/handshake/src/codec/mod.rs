use bytes::BytesMut;
use std::marker::PhantomData;

use crate::HandshakeError;

mod ack;
mod syn;
mod synack;
pub use ack::*;
pub use syn::*;
pub use synack::*;

pub type AckCodec<const N: u64> = ProtocolCodec<crate::proto::handshake::Ack, Ack<N>>;
pub type SynCodec<const N: u64> = ProtocolCodec<crate::proto::handshake::Syn, Syn<N>>;
pub type SynAckCodec<const N: u64> = ProtocolCodec<crate::proto::handshake::SynAck, SynAck<N>>;

/// Generic codec for protobuf messages that can be converted to/from a specific protocol type
pub struct ProtocolCodec<Proto, Protocol>(
    quick_protobuf_codec::Codec<Proto>,
    std::marker::PhantomData<Protocol>,
);

impl<Proto, Protocol> ProtocolCodec<Proto, Protocol> {
    pub fn new(max_packet_size: usize) -> Self {
        Self(
            quick_protobuf_codec::Codec::new(max_packet_size),
            PhantomData,
        )
    }
}

impl<Proto, Protocol> asynchronous_codec::Encoder for ProtocolCodec<Proto, Protocol>
where
    Proto: quick_protobuf::MessageWrite,
    Protocol: Into<Proto>,
{
    type Item<'a> = Protocol;
    type Error = HandshakeError;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        self.0.encode(item.into(), dst).map_err(Into::into)
    }
}

impl<Proto, Protocol> asynchronous_codec::Decoder for ProtocolCodec<Proto, Protocol>
where
    Proto: for<'a> quick_protobuf::MessageRead<'a>,
    Protocol: TryFrom<Proto, Error = HandshakeError>,
{
    type Item = Protocol;
    type Error = HandshakeError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.0.decode(src)?.map(Protocol::try_from).transpose()
    }
}

// Add From implementation for the codec error
impl From<quick_protobuf_codec::Error> for HandshakeError {
    fn from(error: quick_protobuf_codec::Error) -> Self {
        HandshakeError::Protocol(error.to_string())
    }
}
