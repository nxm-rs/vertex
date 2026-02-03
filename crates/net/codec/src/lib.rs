mod utils;
pub use utils::{
    current_unix_timestamp, current_unix_timestamp_nanos, decode_u256_be, encode_u256_be,
};

/// Test helper macro for verifying protobuf roundtrip encoding.
///
/// Encodes a message to proto format and decodes it back, asserting equality.
/// The message type must implement `ProtoMessage` and `Clone + PartialEq + Debug`.
///
/// # Example
///
/// ```ignore
/// use vertex_net_codec::assert_proto_roundtrip;
///
/// #[test]
/// fn test_my_message() {
///     let msg = MyMessage::new(42);
///     assert_proto_roundtrip!(msg);
/// }
/// ```
#[macro_export]
macro_rules! assert_proto_roundtrip {
    ($msg:expr) => {{
        let original = $msg;
        let proto = original.clone().into_proto();
        let decoded =
            <_ as $crate::ProtoMessage>::from_proto(proto).expect("proto decoding should succeed");
        assert_eq!(
            original, decoded,
            "roundtrip encoding should preserve message"
        );
    }};
}

use std::marker::PhantomData;

use bytes::BytesMut;

/// A message type with a protobuf wire representation.
///
/// This trait captures the relationship between a domain type and its protobuf
/// wire format, enabling codec types to use associated types instead of
/// redundant type parameters.
///
/// # Example
///
/// ```ignore
/// impl ProtoMessage for Ping {
///     type Proto = proto::Ping;
///     type DecodeError = PingpongCodecError;
///
///     fn into_proto(self) -> Self::Proto {
///         proto::Ping { greeting: self.greeting }
///     }
///
///     fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
///         Ok(Self { greeting: proto.greeting })
///     }
/// }
/// ```
pub trait ProtoMessage: Sized {
    /// The protobuf message type for wire serialization.
    type Proto: quick_protobuf::MessageWrite + for<'a> quick_protobuf::MessageRead<'a>;

    /// The error type when decoding fails.
    type DecodeError;

    /// Convert to protobuf wire format for encoding.
    fn into_proto(self) -> Self::Proto;

    /// Convert from protobuf wire format (decoding).
    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError>;
}

/// A message type requiring runtime context for encoding and decoding.
///
/// This is used for protocols where encoding/decoding requires runtime information
/// that isn't available in the domain type itself (e.g., `network_id`).
///
/// Context is available on both encode and decode:
/// - **Encode**: Context provides values needed in the wire format but not stored in domain type
/// - **Decode**: Context provides expected values for validation
///
/// # Example
///
/// ```ignore
/// impl ProtoMessageWithContext<u64> for Ack {
///     type Proto = proto::Ack;
///     type DecodeError = CodecError;
///
///     fn into_proto_with_context(self, network_id: &u64) -> Self::Proto {
///         proto::Ack {
///             network_id: *network_id,
///             // ... rest of encoding
///         }
///     }
///
///     fn from_proto_with_context(proto: Self::Proto, network_id: &u64) -> Result<Self, Self::DecodeError> {
///         if proto.network_id != *network_id {
///             return Err(CodecError::NetworkIDMismatch);
///         }
///         // ... rest of decoding
///     }
/// }
/// ```
pub trait ProtoMessageWithContext<Ctx>: Sized {
    /// The protobuf message type for wire serialization.
    type Proto: quick_protobuf::MessageWrite + for<'a> quick_protobuf::MessageRead<'a>;

    /// The error type when decoding fails.
    type DecodeError;

    /// Convert to protobuf wire format for encoding with the given context.
    fn into_proto_with_context(self, ctx: &Ctx) -> Self::Proto;

    /// Convert from protobuf wire format with the given context.
    fn from_proto_with_context(proto: Self::Proto, ctx: &Ctx) -> Result<Self, Self::DecodeError>;
}

/// A codec for protobuf-based protocol messages.
///
/// This codec handles encoding/decoding for types implementing [`ProtoMessage`].
///
/// # Type Parameters
///
/// - `M`: The domain message type (must implement `ProtoMessage`)
/// - `E`: The error type for the codec
///
/// # Example
///
/// ```ignore
/// pub type PingCodec = Codec<Ping, PingpongCodecError>;
///
/// let codec = PingCodec::new(1024);
/// ```
pub struct Codec<M: ProtoMessage, E> {
    inner: quick_protobuf_codec::Codec<M::Proto>,
    _phantom: PhantomData<E>,
}

impl<M: ProtoMessage, E> Codec<M, E> {
    /// Create a new codec with the given maximum packet size.
    pub fn new(max_packet_size: usize) -> Self {
        Self {
            inner: quick_protobuf_codec::Codec::new(max_packet_size),
            _phantom: PhantomData,
        }
    }
}

impl<M, E> asynchronous_codec::Encoder for Codec<M, E>
where
    M: ProtoMessage,
    quick_protobuf_codec::Error: Into<E>,
    E: From<std::io::Error>,
{
    type Item<'a> = M;
    type Error = E;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        self.inner
            .encode(item.into_proto(), dst)
            .map_err(Into::into)
    }
}

impl<M, E> asynchronous_codec::Decoder for Codec<M, E>
where
    M: ProtoMessage,
    M::DecodeError: Into<E>,
    quick_protobuf_codec::Error: Into<E>,
    E: From<std::io::Error>,
{
    type Item = M;
    type Error = E;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode(src).map_err(Into::into)? {
            Some(proto) => {
                let message = M::from_proto(proto).map_err(Into::into)?;
                Ok(Some(message))
            }
            None => Ok(None),
        }
    }
}

/// A codec that carries validation context for decoding.
///
/// This is useful for protocols where decoding requires runtime information
/// that isn't available in the protobuf message itself (e.g., expected `network_id`).
///
/// # Type Parameters
///
/// - `M`: The domain message type (must implement `ProtoMessageWithContext<Ctx>`)
/// - `E`: The error type for the codec
/// - `Ctx`: The validation context type (e.g., `u64` for network_id)
///
/// # Example
///
/// ```ignore
/// pub type AckCodec = ValidatedCodec<Ack, CodecError, u64>;
///
/// let codec = AckCodec::new(1024, expected_network_id);
/// ```
pub struct ValidatedCodec<M, E, Ctx>
where
    M: ProtoMessageWithContext<Ctx>,
{
    inner: quick_protobuf_codec::Codec<M::Proto>,
    context: Ctx,
    _phantom: PhantomData<E>,
}

impl<M, E, Ctx> ValidatedCodec<M, E, Ctx>
where
    M: ProtoMessageWithContext<Ctx>,
{
    /// Create a new validated codec with the given context.
    pub fn new(max_packet_size: usize, context: Ctx) -> Self {
        Self {
            inner: quick_protobuf_codec::Codec::new(max_packet_size),
            context,
            _phantom: PhantomData,
        }
    }

    /// Returns a reference to the validation context.
    pub fn context(&self) -> &Ctx {
        &self.context
    }
}

impl<M, E, Ctx> asynchronous_codec::Encoder for ValidatedCodec<M, E, Ctx>
where
    M: ProtoMessageWithContext<Ctx>,
    quick_protobuf_codec::Error: Into<E>,
    E: From<std::io::Error>,
{
    type Item<'a> = M;
    type Error = E;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        self.inner
            .encode(item.into_proto_with_context(&self.context), dst)
            .map_err(Into::into)
    }
}

impl<M, E, Ctx> asynchronous_codec::Decoder for ValidatedCodec<M, E, Ctx>
where
    M: ProtoMessageWithContext<Ctx>,
    M::DecodeError: Into<E>,
    quick_protobuf_codec::Error: Into<E>,
    E: From<std::io::Error>,
{
    type Item = M;
    type Error = E;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode(src).map_err(Into::into)? {
            Some(proto) => {
                let message =
                    M::from_proto_with_context(proto, &self.context).map_err(Into::into)?;
                Ok(Some(message))
            }
            None => Ok(None),
        }
    }
}

/// A generic error type for protocol codecs.
///
/// This provides the common error variants shared by all protocol codecs:
/// - `Protocol` - for protobuf encoding/decoding errors
/// - `Io` - for I/O errors during read/write
/// - `Domain` - for protocol-specific errors (parameterized by `E`)
///
/// # Type Parameter
///
/// - `E`: Protocol-specific error type. Use `std::convert::Infallible` for
///   protocols with no domain-specific errors.
///
/// # Example
///
/// ```ignore
/// // Simple protocol with no custom errors:
/// type PingCodecError = ProtocolCodecError;
///
/// // Protocol with custom errors:
/// #[derive(Debug, thiserror::Error)]
/// pub enum RetrievalError {
///     #[error("Invalid chunk address length: expected 32, got {0}")]
///     InvalidAddressLength(usize),
/// }
/// type RetrievalCodecError = ProtocolCodecError<RetrievalError>;
/// ```
#[derive(Debug, thiserror::Error)]
pub enum ProtocolCodecError<E = std::convert::Infallible> {
    /// Protocol-level error (invalid message format, protobuf errors, etc.)
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// I/O error during read/write operations.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Protocol-specific domain error.
    #[error(transparent)]
    Domain(E),
}

impl<E> From<quick_protobuf_codec::Error> for ProtocolCodecError<E> {
    fn from(error: quick_protobuf_codec::Error) -> Self {
        ProtocolCodecError::Protocol(error.to_string())
    }
}

impl<E> ProtocolCodecError<E> {
    /// Create a protocol error with a message.
    pub fn protocol(msg: impl Into<String>) -> Self {
        ProtocolCodecError::Protocol(msg.into())
    }

    /// Create a domain-specific error.
    pub fn domain(error: E) -> Self {
        ProtocolCodecError::Domain(error)
    }

    /// Map the domain error type to a different type.
    pub fn map_domain<F, E2>(self, f: F) -> ProtocolCodecError<E2>
    where
        F: FnOnce(E) -> E2,
    {
        match self {
            ProtocolCodecError::Protocol(msg) => ProtocolCodecError::Protocol(msg),
            ProtocolCodecError::Io(e) => ProtocolCodecError::Io(e),
            ProtocolCodecError::Domain(e) => ProtocolCodecError::Domain(f(e)),
        }
    }
}
