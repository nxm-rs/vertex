use bytes::BytesMut;
use vertex_net_codec::ProtocolCodec;

mod ack;
mod error;
mod syn;
mod synack;
pub use ack::{ack_from_proto, Ack};
pub use syn::Syn;
pub use synack::{synack_from_proto, SynAck};

pub use error::CodecError;

/// Codec for Syn messages (no network_id validation needed).
pub type SynCodec = ProtocolCodec<crate::proto::handshake::Syn, Syn, CodecError>;

/// Codec for Ack messages with network_id validation.
pub struct AckCodec {
    inner: quick_protobuf_codec::Codec<crate::proto::handshake::Ack>,
    expected_network_id: u64,
}

impl AckCodec {
    pub fn new(max_packet_size: usize, expected_network_id: u64) -> Self {
        Self {
            inner: quick_protobuf_codec::Codec::new(max_packet_size),
            expected_network_id,
        }
    }
}

impl asynchronous_codec::Encoder for AckCodec {
    type Item<'a> = Ack;
    type Error = CodecError;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let proto: crate::proto::handshake::Ack = item.into();
        self.inner.encode(proto, dst).map_err(Into::into)
    }
}

impl asynchronous_codec::Decoder for AckCodec {
    type Item = Ack;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode(src).map_err(CodecError::from)? {
            Some(proto) => Ok(Some(ack_from_proto(proto, self.expected_network_id)?)),
            None => Ok(None),
        }
    }
}

/// Codec for SynAck messages with network_id validation.
pub struct SynAckCodec {
    inner: quick_protobuf_codec::Codec<crate::proto::handshake::SynAck>,
    expected_network_id: u64,
}

impl SynAckCodec {
    pub fn new(max_packet_size: usize, expected_network_id: u64) -> Self {
        Self {
            inner: quick_protobuf_codec::Codec::new(max_packet_size),
            expected_network_id,
        }
    }
}

impl asynchronous_codec::Encoder for SynAckCodec {
    type Item<'a> = SynAck;
    type Error = CodecError;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let proto: crate::proto::handshake::SynAck = item.into();
        self.inner.encode(proto, dst).map_err(Into::into)
    }
}

impl asynchronous_codec::Decoder for SynAckCodec {
    type Item = SynAck;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode(src).map_err(CodecError::from)? {
            Some(proto) => Ok(Some(synack_from_proto(proto, self.expected_network_id)?)),
            None => Ok(None),
        }
    }
}
