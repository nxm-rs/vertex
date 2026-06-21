//! Decoupling seams between the [`Puller`](crate::Puller) and the live node.
//!
//! The puller drives an abstract [`PullsyncControl`] (bridged to the live
//! `PullsyncBehaviour` by the node) and consumes [`PullsyncEvent`]s off an mpsc
//! receiver, gates on a [`ReadinessGate`], selects neighbours through a
//! [`NeighbourSource`], and admits chunks through a [`ReserveAdmit`] put seam.

use std::future::Future;

use libp2p::PeerId;
use vertex_swarm_api::SwarmResult;
use vertex_swarm_primitives::{Bin, OverlayAddress, StampedChunk};

pub use vertex_swarm_storer_behaviour::PullsyncEvent;

/// Outbound command surface the puller drives, bridged to `PullsyncBehaviour`.
///
/// Each call opens an outbound substream against `peer`; its result arrives as a
/// [`PullsyncEvent`] on the puller's event receiver, echoing `request_id` so the
/// puller can discard a stale reply from a prior, timed-out command.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait PullsyncControl: Send + Sync {
    /// Open the cursor handshake against `peer`.
    fn fetch_cursors(&self, peer: PeerId, request_id: u64);

    /// Open a range exchange against `peer` for `bin` from `start`.
    fn sync_range(&self, peer: PeerId, request_id: u64, bin: Bin, start: u64);
}

/// Readiness gate the puller awaits before each sync pass.
///
/// Bridged to `TopologyHandle::wait_until_neighborhood_ready`; abstracted so the
/// loop is testable without a live topology.
pub trait ReadinessGate: Send + Sync {
    /// Resolve once the neighbourhood is ready for pull-syncing.
    fn wait_ready(&self) -> impl Future<Output = ()> + Send;
}

/// A neighbour to pull-sync from, with the bins in scope for it.
#[derive(Debug, Clone)]
pub struct SyncTarget {
    /// libp2p identity for the [`PullsyncControl`] commands.
    pub peer: PeerId,
    /// Overlay identity keying the persisted intervals.
    pub overlay: OverlayAddress,
    /// Neighbourhood bins to sync, shallowest first.
    pub bins: Vec<Bin>,
}

/// Source of the neighbours and bins to sync on each pass.
///
/// Bridged to the topology handle (depth, neighbourhood bins, connected
/// neighbours and their peer ids).
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait NeighbourSource: Send + Sync {
    /// The current set of sync targets.
    fn targets(&self) -> Vec<SyncTarget>;
}

/// Reserve admission put seam.
///
/// A blanket impl bridges any [`SwarmLocalStore`] (the storer reserve), so the
/// puller crate depends on no storer backend.
///
/// [`SwarmLocalStore`]: vertex_swarm_api::SwarmLocalStore
pub trait ReserveAdmit: Send + Sync {
    /// Admit a verified, stamped chunk to the reserve.
    fn admit(&self, chunk: StampedChunk) -> SwarmResult<()>;
}

/// Bridge any [`SwarmLocalStore`] to the put seam, converting the chunk to the
/// always-stamped [`CachedChunk`] the reserve expects.
///
/// [`SwarmLocalStore`]: vertex_swarm_api::SwarmLocalStore
/// [`CachedChunk`]: vertex_swarm_primitives::CachedChunk
impl<S: vertex_swarm_api::SwarmLocalStore> ReserveAdmit for S {
    fn admit(&self, chunk: StampedChunk) -> SwarmResult<()> {
        self.put(vertex_swarm_primitives::CachedChunk::from(chunk))
    }
}
