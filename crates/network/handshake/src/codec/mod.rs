use bytes::BytesMut;
use std::marker::PhantomData;

mod ack;
mod error;
mod syn;
mod synack;
pub use ack::Ack;
pub use syn::Syn;
pub use synack::SynAck;

pub use error::CodecError;

pub type AckCodec<const N: u64> = ProtocolCodec<crate::proto::handshake::Ack, Ack<N>, CodecError>;
pub type SynCodec<const N: u64> = ProtocolCodec<crate::proto::handshake::Syn, Syn<N>, CodecError>;
pub type SynAckCodec<const N: u64> =
    ProtocolCodec<crate::proto::handshake::SynAck, SynAck<N>, CodecError>;

impl From<quick_protobuf_codec::Error> for CodecError {
    fn from(error: quick_protobuf_codec::Error) -> Self {
        CodecError::Protocol(error.to_string())
    }
}

pub struct ProtocolCodec<Proto, Protocol, E>(
    quick_protobuf_codec::Codec<Proto>,
    std::marker::PhantomData<(Protocol, E)>,
);

impl<Proto, Protocol, E> ProtocolCodec<Proto, Protocol, E> {
    pub fn new(max_packet_size: usize) -> Self {
        Self(
            quick_protobuf_codec::Codec::new(max_packet_size),
            PhantomData,
        )
    }
}

impl<Proto, Protocol, E> asynchronous_codec::Encoder for ProtocolCodec<Proto, Protocol, E>
where
    Proto: quick_protobuf::MessageWrite,
    Protocol: Into<Proto>,
    quick_protobuf_codec::Error: Into<E>,
    E: From<std::io::Error>,
{
    type Item<'a> = Protocol;
    type Error = E;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        self.0.encode(item.into(), dst).map_err(Into::into)
    }
}

impl<Proto, Protocol, PE, E> asynchronous_codec::Decoder for ProtocolCodec<Proto, Protocol, E>
where
    Proto: for<'a> quick_protobuf::MessageRead<'a>,
    Protocol: TryFrom<Proto, Error = PE>,
    PE: Into<E>,
    quick_protobuf_codec::Error: Into<E>,
    E: From<std::io::Error>,
{
    type Item = Protocol;
    type Error = E;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.0.decode(src).map_err(Into::into)? {
            Some(proto) => match Protocol::try_from(proto) {
                Ok(protocol) => Ok(Some(protocol)),
                Err(e) => Err(e.into()),
            },
            None => Ok(None),
        }
    }
}
