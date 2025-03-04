use vertex_network_codec::ProtocolCodec;

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
