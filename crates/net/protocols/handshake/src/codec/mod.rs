//! Codec types for handshake protocol messages.

use vertex_net_codec::Codec;

use crate::HandshakeError;

mod ack;
mod syn;
mod synack;

pub use ack::Ack;
pub use syn::Syn;
pub use synack::SynAck;

/// Codec for Syn messages.
pub type SynCodec = Codec<Syn, HandshakeError>;

/// Codec for SynAck messages with network_id validation.
pub type SynAckCodec = vertex_net_codec::ValidatedCodec<SynAck, HandshakeError, u64>;

/// Codec for Ack messages with network_id validation.
pub type AckCodec = vertex_net_codec::ValidatedCodec<Ack, HandshakeError, u64>;
