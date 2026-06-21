//! Pullsync trait surface: server snapshot, puller interval persistence, and the
//! chunk-admission verifier seam.

use nectar_primitives::Bin;
use vertex_swarm_primitives::{OverlayAddress, StampedChunk};

use crate::SwarmResult;

use super::BinCursorStore;

/// Server-side snapshot the cursor handshake and range responder read.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait PullStorage: BinCursorStore {
    /// Reserve generation marker; changes only on reserve recreate, so a new
    /// value tells a puller its persisted cursors are stale.
    fn reserve_epoch(&self) -> u64;
}

/// Puller-side persistence of sync progress per `(peer, bin)`.
///
/// A stored `peer_epoch` that no longer matches the peer's advertised
/// [`reserve_epoch`](PullStorage::reserve_epoch) means the puller must reset that
/// peer's intervals to `0`.
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

    /// Clear every per-bin interval for `peer` and record `epoch`, atomically.
    ///
    /// The epoch is the commit barrier: a recorded epoch matching the advertised
    /// one must never coexist with a stale interval, so the interval clear and the
    /// epoch write commit together or not at all. No non-atomic default impl over
    /// the per-field setters is permitted.
    fn reset_peer(&self, peer: &OverlayAddress, epoch: u64) -> SwarmResult<()>;
}

/// Why a delivered chunk was refused admission to the reserve.
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
/// batch funding, so an interim signature-only impl is valid.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait PullChunkVerifier: Send + Sync {
    /// Verify a delivered chunk before it enters the reserve.
    fn verify(&self, chunk: &StampedChunk) -> Result<(), VerifyError>;
}
