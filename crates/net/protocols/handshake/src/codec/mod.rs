//! Codec types for handshake protocol messages.

use vertex_net_codec::Codec;

mod ack;
pub mod error;
mod syn;
mod synack;

pub use ack::Ack;
pub use syn::Syn;
pub use synack::SynAck;

pub use error::{CodecError, HandshakeCodecDomainError};

/// Codec for Syn messages.
pub type SynCodec = Codec<Syn, CodecError>;

/// Codec for SynAck messages with network_id validation.
pub type SynAckCodec = vertex_net_codec::ValidatedCodec<SynAck, CodecError, u64>;

/// Codec for Ack messages with network_id validation.
pub type AckCodec = vertex_net_codec::ValidatedCodec<Ack, CodecError, u64>;
