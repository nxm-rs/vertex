//! Codec utilities for protobuf-based network protocols.

mod framed;
pub use framed::{FramedProto, StreamClosed};

mod utils;
pub use utils::{current_unix_timestamp_nanos, decode_u256_be, encode_u256_be};

/// Direct protobuf codec for types that don't need domain wrapper conversion.
pub(crate) type ProtoCodec<T> = quick_protobuf_codec::Codec<T>;

/// Test helper macro for verifying protobuf roundtrip encoding.
///
/// Encodes a message to proto format and decodes it back, asserting equality.
/// The message type must implement `ProtoMessage` and `Clone + PartialEq + Debug`.
#[macro_export]
macro_rules! assert_proto_roundtrip {
    ($msg:expr) => {{
        let original = $msg;
        let proto = original
            .clone()
            .into_proto()
            .expect("proto encoding should succeed");
        let decoded =
            <_ as $crate::ProtoMessage>::from_proto(proto).expect("proto decoding should succeed");
        assert_eq!(
            original, decoded,
            "roundtrip encoding should preserve message"
        );
    }};
}

/// Generates a protocol error enum with common variants for protobuf-based protocols.
///
/// All protocol error types share `ConnectionClosed`, `Protobuf`, and `Io` variants
/// plus a `From<Infallible>` impl. This macro generates those, and you can add
/// protocol-specific variants after the macro invocation.
///
/// # Example
///
/// ```ignore
/// vertex_net_codec::protocol_error! {
///     /// Pushsync protocol errors.
///     pub enum PushsyncError {
///         /// Invalid chunk address length.
///         #[error("invalid chunk address length: expected 32, got {0}")]
///         #[strum(serialize = "invalid_address_length")]
///         InvalidAddressLength(usize),
///     }
/// }
/// ```
#[macro_export]
macro_rules! protocol_error {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident $( ( $($field:ty),* $(,)? ) )? $( { $($struct_field:ident : $struct_ty:ty),* $(,)? } )?,
            )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, ::thiserror::Error, ::strum::IntoStaticStr)]
        #[strum(serialize_all = "snake_case")]
        $vis enum $name {
            /// Connection closed before operation completed.
            #[error("connection closed")]
            ConnectionClosed,

            $(
                $(#[$variant_meta])*
                $variant $( ( $($field),* ) )? $( { $($struct_field : $struct_ty),* } )?,
            )*

            /// Protobuf encoding/decoding error.
            #[error("protobuf error: {0}")]
            #[strum(serialize = "protobuf_error")]
            Protobuf(#[from] ::quick_protobuf_codec::Error),

            /// I/O error during stream operations.
            #[error("io error: {0}")]
            #[strum(serialize = "io_error")]
            Io(#[from] ::std::io::Error),
        }

        impl From<::std::convert::Infallible> for $name {
            fn from(never: ::std::convert::Infallible) -> Self {
                match never {}
            }
        }
    };
}

use std::marker::PhantomData;

use bytes::BytesMut;

/// A message type with a protobuf wire representation.
///
/// This trait captures the relationship between a domain type and its protobuf
/// wire format, enabling codec types to use associated types instead of
/// redundant type parameters.
pub trait ProtoMessage: Sized {
    /// The protobuf message type for wire serialization.
    type Proto: quick_protobuf::MessageWrite + for<'a> quick_protobuf::MessageRead<'a>;

    /// The error type when encoding fails. Use `std::convert::Infallible` for infallible encoding.
    type EncodeError;

    /// The error type when decoding fails.
    type DecodeError;

    /// Convert to protobuf wire format for encoding.
    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError>;

    /// Convert from protobuf wire format (decoding).
    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError>;
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
/// pub type DeliveryCodec = Codec<Delivery, PushsyncError>;
///
/// let codec = DeliveryCodec::new(1024);
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
    M::EncodeError: Into<E>,
    quick_protobuf_codec::Error: Into<E>,
    E: From<std::io::Error>,
{
    type Item<'a> = M;
    type Error = E;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let proto = item.into_proto().map_err(Into::into)?;
        self.inner.encode(proto, dst).map_err(Into::into)
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
