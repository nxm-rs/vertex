//! The forwarder seam: relay a retrieval or a pushsync to a closer peer.
//!
//! Inbound serving is handler-inline: each inbound retrieval or pushsync request
//! becomes one self-contained future, with the substream itself as the
//! correlation (mirroring the outbound model). When the local cache cannot
//! answer a retrieval, or for every pushsync, the handler hands off to a
//! [`Forwarder`] that relays to a closer peer and returns the result.
//!
//! In the cache-only client this seam is stubbed: [`StubForwarder`] always
//! returns [`ForwardError::NoCloserPeer`], so a cache miss and every pushsync
//! reset the inbound substream (the reference reads a reset as a failed request
//! at that hop). The real multi-hop relay (closest-peer selection excluding the
//! requester, hop and loop bounds, two-leg accounting, relay and shallow-receipt
//! verification) is filled in separately and reuses the existing self-contained
//! outbound futures.

use futures::future::BoxFuture;
use vertex_swarm_api::PushReceipt;
use vertex_swarm_primitives::{OverlayAddress, StampedChunk};

/// Why a forward could not complete.
///
/// The reason is intentionally coarse: the handler only needs to know the
/// forward did not produce a chunk or receipt so it can reset the inbound
/// substream. A real forwarder will carry richer diagnostics for its own
/// metrics, but the inbound serving path treats every failure as a reset.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum ForwardError {
    /// No peer closer to the target than the requester is available to relay to.
    #[error("no closer peer to forward to")]
    NoCloserPeer,
}

/// Relays a retrieval or a pushsync to a closer peer on behalf of an inbound
/// request.
///
/// `exclude` is the requester or pusher, passed so the forwarder never relays
/// back to the peer that asked (loop prevention). The returned futures are
/// `'static`, boxed, and `Send` so the handler can hold them in its inbound set:
/// a libp2p `ConnectionHandler` is `Send` on both native and wasm (the browser
/// `Stream` is itself `Send`), so the inbound serving futures are `Send` too.
pub(crate) trait Forwarder: Send + Sync {
    /// Retrieve `address` from a closer peer, excluding `exclude`.
    fn retrieve(
        &self,
        address: nectar_primitives::ChunkAddress,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<StampedChunk, ForwardError>>;

    /// Push `chunk` to a closer peer, excluding `exclude`, returning the
    /// storer's receipt to relay verbatim.
    fn push(
        &self,
        chunk: StampedChunk,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<PushReceipt, ForwardError>>;
}

/// The cache-only client forwarder: every relay fails with
/// [`ForwardError::NoCloserPeer`].
///
/// A cache miss therefore resets the inbound retrieval substream and every
/// inbound pushsync resets too, which is the correct behaviour for a node that
/// holds no reserve and takes no custody. The real relay is filled in
/// separately.
pub(crate) struct StubForwarder;

impl Forwarder for StubForwarder {
    fn retrieve(
        &self,
        _address: nectar_primitives::ChunkAddress,
        _exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<StampedChunk, ForwardError>> {
        Box::pin(async { Err(ForwardError::NoCloserPeer) })
    }

    fn push(
        &self,
        _chunk: StampedChunk,
        _exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<PushReceipt, ForwardError>> {
        Box::pin(async { Err(ForwardError::NoCloserPeer) })
    }
}
