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

    /// Whether the locally observed neighbourhood depth is credible.
    ///
    /// The depth is an atomic that begins at zero on a fresh, sparse, or
    /// just-restarted node and only reflects a real responsibility boundary once
    /// the neighbourhood has saturated. Custody-receipt depth checks must not
    /// anchor their floor to a non-credible depth: the receipt's declared radius
    /// is unsigned wire data and would otherwise become the sole,
    /// attacker-controlled bar (see the pushsync receipt depth policy). Returns
    /// true once the neighbourhood is saturated.
    fn neighbourhood_credible(&self) -> bool;

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

/// Direction of a peer connection, mirrored here so the diagnostics surface does
/// not depend on the peer-registry crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerConnectionDirection {
    /// We dialed the peer.
    Outbound,
    /// The peer dialed us.
    Inbound,
}

/// Trust level applied to a peer, mirrored here for the diagnostics surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerTrustLevel {
    /// No special standing.
    Normal,
    /// Loopback, link-local, or same-subnet peer.
    LocalSubnet,
    /// Explicitly configured trusted peer.
    Trusted,
}

/// Per-peer diagnostics assembled from the peer manager and connection registry.
#[derive(Debug, Clone)]
pub struct PeerDiagnostics {
    /// Overlay address.
    pub overlay: OverlayAddress,
    /// libp2p peer id, if currently mapped in the connection registry.
    pub peer_id: Option<libp2p::PeerId>,
    /// Known multiaddrs from the stored peer record.
    pub multiaddrs: Vec<libp2p::Multiaddr>,
    /// First IP parsed from the peer's multiaddrs, if any.
    pub ip: Option<std::net::IpAddr>,
    /// Proximity order / bin of this peer relative to the local overlay.
    pub proximity_order: u8,
    /// Current reputation score, if tracked.
    pub score: Option<f64>,
    /// Whether the peer currently has a handshake-complete connection.
    pub connected: bool,
    /// Unix seconds at which the current connection completed its handshake.
    pub connected_since: Option<u64>,
    /// Seconds since the current connection completed its handshake.
    pub uptime_secs: Option<u64>,
    /// Direction of the current connection.
    pub direction: Option<PeerConnectionDirection>,
    /// Trust level applied to this peer.
    pub trust: PeerTrustLevel,
    /// Whether a completed handshake has verified this peer in this process.
    pub verified: bool,
}

/// Read-only peer-administration diagnostics over the topology, backing the
/// `ListPeers` operator endpoint.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmTopologyAdmin: Send + Sync {
    /// Per-peer diagnostics for every peer the node knows; when `connected_only`
    /// is true, only peers with a current handshake-complete connection.
    fn peer_diagnostics(&self, connected_only: bool) -> Vec<PeerDiagnostics>;
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
