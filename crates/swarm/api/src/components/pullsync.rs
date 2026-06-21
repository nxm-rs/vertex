//! Pullsync trait surface: the server snapshot, puller interval persistence, and
//! the chunk-admission verifier seam.

use nectar_primitives::Bin;
use vertex_swarm_primitives::{OverlayAddress, StampedChunk};

use crate::SwarmResult;

use super::BinCursorStore;

/// Server-side snapshot the cursor handshake and range responder read.
///
/// The two primitives the handshake and range responder compose with: the
/// inherited per-bin insertion-order surface ([`bin_cursor`](BinCursorStore::bin_cursor)
/// for the handshake, [`scan_bin_from`](BinCursorStore::scan_bin_from) for the
/// Offer page, [`get`](super::SwarmLocalStore::get) for the Delivery body) plus a
/// `reserve_epoch` creation marker. The epoch changes only when the reserve is
/// recreated, so a puller that observes a new epoch knows its persisted cursors
/// are stale. Cursor enumeration is the caller's, keyed by the typed [`Bin`].
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait PullStorage: BinCursorStore {
    /// Creation marker for the current reserve. A change invalidates every
    /// cursor a puller persisted against the old reserve.
    fn reserve_epoch(&self) -> u64;
}

/// Puller-side persistence of sync progress per `(peer, bin)`.
///
/// Tracks the last synced insertion sequence per bin and the last epoch seen per
/// peer. A `peer_epoch` that no longer matches the peer's advertised
/// [`reserve_epoch`](PullStorage::reserve_epoch) means the peer's reserve was
/// recreated, so the puller resets that peer's per-bin intervals to `0`.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait IntervalStore: Send + Sync {
    /// Last synced insertion sequence for `peer` in `bin` (`0` if never synced).
    fn interval(&self, peer: &OverlayAddress, bin: Bin) -> SwarmResult<u64>;

    /// Record the last synced insertion sequence for `peer` in `bin`.
    fn set_interval(&self, peer: &OverlayAddress, bin: Bin, binid: u64) -> SwarmResult<()>;

    /// Last reserve epoch seen for `peer`, or `None` if never recorded.
    fn peer_epoch(&self, peer: &OverlayAddress) -> SwarmResult<Option<u64>>;

    /// Record the last reserve epoch seen for `peer`.
    fn set_peer_epoch(&self, peer: &OverlayAddress, epoch: u64) -> SwarmResult<()>;
}

/// Why a delivered chunk was refused admission to the reserve.
///
/// The label-carrying refusal reasons for the puller's admission metric.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum VerifyError {
    /// The stamp signature does not recover to the batch owner.
    #[error("invalid stamp signature")]
    InvalidSignature,

    /// The stamp references a batch that is expired or unknown to the node.
    #[error("postage batch is expired or unknown")]
    UnknownBatch,

    /// The batch exists but no longer covers its cumulative payout.
    #[error("postage batch funding is insufficient")]
    InsufficientFunding,

    /// The chunk or its stamp is structurally malformed.
    #[error("chunk or stamp is malformed")]
    Malformed,
}

/// Admission gate the puller runs before accepting a delivered chunk.
///
/// A pluggable seam: the full check is stamp signature recovery plus on-chain
/// batch funding, but the batch-store and funding machinery are injected
/// separately, so an interim signature-only implementation satisfies the
/// signature arm while leaving [`UnknownBatch`](VerifyError::UnknownBatch) and
/// [`InsufficientFunding`](VerifyError::InsufficientFunding) to the injected
/// verifier. Synchronous to match the stateless stamp validators it composes.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait PullChunkVerifier: Send + Sync {
    /// Verify a delivered chunk before it enters the reserve.
    fn verify(&self, chunk: &StampedChunk) -> Result<(), VerifyError>;
}
