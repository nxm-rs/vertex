//! Send/recv helpers for protobuf-framed streams.

use asynchronous_codec::Framed;
use futures::{SinkExt, TryStreamExt};
use quick_protobuf::{MessageRead, MessageWrite};

use crate::ProtoCodec;

/// Stream closed before a complete message was received.
#[derive(Debug, Clone, Copy)]
pub struct StreamClosed;

impl std::fmt::Display for StreamClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stream closed before message received")
    }
}

impl std::error::Error for StreamClosed {}

/// Length-delimited protobuf send/recv with a compile-time buffer size.
///
/// # Const Parameters
///
/// - `BUF`: Maximum protobuf frame size in bytes.
///
/// # Usage
///
/// ```ignore
/// use vertex_net_codec::FramedProto;
///
/// type Handshake = FramedProto<2048>;
///
/// // M inferred from argument, E inferred from ?
/// let stream = Handshake::send(stream, syn_msg).await?;
/// // Only M needs specifying for recv
/// let (ack, stream) = Handshake::recv::<Ack, _, _>(stream).await?;
/// ```
pub struct FramedProto<const BUF: usize>;

impl<const BUF: usize> FramedProto<BUF> {
    /// Send a protobuf message over a length-delimited framed stream.
    pub async fn send<M, E, S>(stream: S, msg: M) -> Result<S, E>
    where
        M: MessageWrite + for<'a> MessageRead<'a> + Default,
        E: From<quick_protobuf_codec::Error>,
        S: futures::AsyncRead + futures::AsyncWrite + Unpin,
    {
        let codec = ProtoCodec::<M>::new(BUF);
        let mut framed = Framed::new(stream, codec);
        framed.send(msg).await?;
        Ok(framed.into_inner())
    }

    /// Receive a protobuf message from a length-delimited framed stream.
    ///
    /// Returns [`StreamClosed`] (via `From<StreamClosed>`) if the stream ends
    /// before a complete message is received.
    pub async fn recv<M, E, S>(stream: S) -> Result<(M, S), E>
    where
        M: MessageWrite + for<'a> MessageRead<'a> + Default,
        E: From<quick_protobuf_codec::Error> + From<StreamClosed>,
        S: futures::AsyncRead + futures::AsyncWrite + Unpin,
    {
        let codec = ProtoCodec::<M>::new(BUF);
        let mut framed = Framed::new(stream, codec);
        let msg = framed.try_next().await?.ok_or(StreamClosed)?;
        Ok((msg, framed.into_inner()))
    }
}
