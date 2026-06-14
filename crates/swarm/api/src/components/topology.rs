//! Topology and neighborhood awareness using overlay addresses.

use core::future::Future;
use std::sync::Arc;
use std::vec::Vec;

use nectar_primitives::ChunkAddress;

use crate::{PeerReporter, SwarmIdentity};
use vertex_swarm_primitives::{Bin, NeighborhoodDepth, OverlayAddress};

/// Bin sizes for topology routing (one per proximity order).
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmTopologyBins: Send + Sync {
    /// Get bin sizes for each proximity order (0-31).
    ///
    /// Returns a vector of `(connected, known)` tuples, one per bin.
    fn bin_sizes(&self) -> Vec<(usize, usize)>;
}

/// Node identity and state within the topology.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmTopologyState: Send + Sync {
    /// The identity type for this topology.
    type Identity: SwarmIdentity;

    /// Get the identity.
    fn identity(&self) -> &Self::Identity;

    /// Get the current neighborhood depth.
    fn depth(&self) -> NeighborhoodDepth;

    /// Get the node's overlay address.
    fn overlay_address(&self) -> OverlayAddress {
        self.identity().overlay_address()
    }
}

/// Access to the peer-scoring authority behind the topology.
///
/// The peer manager is the single sanctioned scoring path
/// ([`PeerReporter`](crate::PeerReporter)); subsystems wired from the topology
/// handle (the forwarder, the origin upload path) report misbehaving peers
/// through it. This accessor lets those subsystems source the reporter from the
/// same handle they already hold instead of threading it as a separate
/// parameter.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmTopologyReporting: Send + Sync {
    /// The peer-scoring authority for this topology.
    fn reporter(&self) -> Arc<dyn PeerReporter>;
}

/// Routing queries against the topology.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmTopologyRouting: Send + Sync {
    /// Find peers closest to a given address.
    fn closest_to(&self, address: &ChunkAddress, count: usize) -> Vec<OverlayAddress>;

    /// Get peers within our neighborhood at the given depth.
    fn neighbors(&self, depth: NeighborhoodDepth) -> Vec<OverlayAddress>;
}

/// Connected peer inspection per bin.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmTopologyPeers: SwarmTopologyBins {
    /// Get connected peer overlay addresses in a specific bin.
    fn connected_peers_in_bin(&self, bin: Bin) -> Vec<OverlayAddress>;

    /// Get connected peer details in a specific bin.
    ///
    /// Returns `(overlay, multiaddrs)` for each connected peer in the bin.
    fn connected_peer_details_in_bin(
        &self,
        bin: Bin,
    ) -> Vec<(OverlayAddress, Vec<libp2p::Multiaddr>)>;
}

/// Connection and storage statistics for topology monitoring.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmTopologyStats: SwarmTopologyBins {
    /// Get the count of currently connected peers.
    fn connected_peers_count(&self) -> usize;

    /// Peers in the bounded routing table (ProximityIndex).
    fn routing_peers_count(&self) -> usize;

    /// Get the count of pending connection attempts.
    fn pending_connections_count(&self) -> usize;

    /// Total peers persisted in the backing store.
    fn stored_peers_count(&self) -> usize;
}

/// Write operations for topology control.
///
/// Consumed only through concrete handle types (never as a trait object), so
/// the methods return `impl Future + Send` natively; the `Send` bounds keep the
/// futures usable from spawned, multi-threaded contexts.
pub trait SwarmTopologyCommands: Send + Sync {
    /// The error type for command failures.
    type Error: std::error::Error + Send + Sync;

    /// Trigger connection to bootnodes and trusted peers.
    #[must_use = "connection failures should be handled"]
    fn connect_bootnodes(&self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Initiate dial to peer address, queuing connection attempt.
    #[must_use = "dial failures should be handled"]
    fn dial(&self, addr: libp2p::Multiaddr)
    -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Disconnect from peer, closing all connections.
    #[must_use = "disconnect failures should be handled"]
    fn disconnect(
        &self,
        peer: OverlayAddress,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Ban a peer and remove from routing.
    #[must_use = "ban failures should be handled"]
    fn ban_peer(
        &self,
        peer: OverlayAddress,
        reason: Option<String>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Flush known peers to persistent storage.
    #[must_use = "save failures should be handled"]
    fn save_peers(&self) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// Full topology interface combining state, routing, peers, and stats.
///
/// Blanket-implemented for any type implementing all four sub-traits.
pub trait SwarmTopology:
    SwarmTopologyState + SwarmTopologyRouting + SwarmTopologyPeers + SwarmTopologyStats
{
}

impl<T> SwarmTopology for T where
    T: SwarmTopologyState + SwarmTopologyRouting + SwarmTopologyPeers + SwarmTopologyStats
{
}
