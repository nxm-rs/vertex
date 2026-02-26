//! Types for dial tracking.

use std::fmt::Debug;
use std::time::Instant;

use libp2p::{Multiaddr, PeerId};

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
    /// When this request was queued (set internally by enqueue/start_dial).
    pub(crate) queued_at: Instant,
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
        }
    }

    /// Create a new dial request without a known application-level Id.
    pub fn without_id(peer_id: PeerId, addrs: Vec<Multiaddr>, data: D) -> Self {
        Self {
            id: None,
            peer_id,
            addrs,
            data,
            queued_at: Instant::now(),
        }
    }

    /// When this request was queued.
    pub fn queued_at(&self) -> Instant {
        self.queued_at
    }
}

/// Returned by `next_dial()` — enough info to create `DialOpts`.
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

/// Result of trying to enqueue a dial request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueResult {
    /// Request was successfully enqueued.
    Enqueued,
    /// An entry with this PeerId is already pending.
    AlreadyPending,
    /// An entry with this PeerId is already in-flight.
    AlreadyInFlight,
    /// The pending queue is full.
    QueueFull,
}

/// Current tracker counts for metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialCounts {
    /// Number of pending requests in the queue.
    pub pending: usize,
    /// Number of in-flight dials.
    pub in_flight: usize,
}

/// Expired entries from cleanup.
#[derive(Debug)]
pub struct CleanupResult<Id, D> {
    /// Pending entries that exceeded the TTL.
    pub expired_pending: Vec<DialRequest<Id, D>>,
    /// In-flight entries that exceeded the timeout.
    pub timed_out_in_flight: Vec<DialRequest<Id, D>>,
}
