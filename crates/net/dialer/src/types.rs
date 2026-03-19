//! Types for dial tracking.

use std::fmt::Debug;
use std::time::Instant;

use libp2p::{Multiaddr, PeerId};

/// Why a dial request could not be enqueued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum EnqueueError {
    #[error("already pending")]
    AlreadyPending,
    #[error("already in-flight")]
    AlreadyInFlight,
    #[error("queue full")]
    QueueFull,
    #[error("in backoff")]
    InBackoff,
    #[error("banned")]
    Banned,
}

/// A dial request queued in the tracker.
#[derive(Debug, Clone)]
pub struct DialRequest<Id, D> {
    /// Application-level peer identifier (may be unknown at dial time).
    pub id: Option<Id>,
    /// libp2p PeerId for the peer (always known).
    pub peer_id: PeerId,
    /// Addresses to dial.
    pub addrs: Vec<Multiaddr>,
    /// Arbitrary data carried with the request.
    pub data: D,
    /// When this request was first created/enqueued.
    pub(crate) queued_at: Instant,
    /// When this request moved to in-flight (`None` while still pending).
    pub(crate) started_at: Option<Instant>,
}

impl<Id, D> DialRequest<Id, D> {
    /// Create a new dial request with a known application-level Id.
    pub fn new(id: Id, peer_id: PeerId, addrs: Vec<Multiaddr>, data: D) -> Self {
        Self {
            id: Some(id),
            peer_id,
            addrs,
            data,
            queued_at: Instant::now(),
            started_at: None,
        }
    }

    /// When this request was first created/enqueued.
    pub fn queued_at(&self) -> Instant {
        self.queued_at
    }

    /// When this request moved to in-flight, or `None` if still pending.
    pub fn started_at(&self) -> Option<Instant> {
        self.started_at
    }

    /// Create a new dial request without a known application-level Id.
    pub fn without_id(peer_id: PeerId, addrs: Vec<Multiaddr>, data: D) -> Self {
        Self {
            id: None,
            peer_id,
            addrs,
            data,
            queued_at: Instant::now(),
            started_at: None,
        }
    }
}

/// Returned by [`DialTracker::next_batch`](crate::DialTracker::next_batch) — enough info to build `DialOpts`.
///
/// The full `DialRequest` (including `D`) stays in the tracker until `resolve()`.
#[derive(Debug, Clone)]
pub struct DialDispatch<Id> {
    /// Application-level peer identifier (if known).
    pub id: Option<Id>,
    /// libp2p PeerId.
    pub peer_id: PeerId,
    /// Addresses to dial.
    pub addrs: Vec<Multiaddr>,
}

/// Expired entries from cleanup.
#[derive(Debug)]
pub struct CleanupResult<Id, D> {
    /// Pending entries that exceeded the TTL.
    pub expired_pending: Vec<DialRequest<Id, D>>,
    /// In-flight entries that exceeded the timeout.
    pub timed_out_in_flight: Vec<DialRequest<Id, D>>,
}
