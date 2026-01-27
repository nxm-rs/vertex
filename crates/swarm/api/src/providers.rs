//! RPC provider traits for Swarm protocol.
//!
//! These traits define the data interfaces that RPC services depend on.
//! They abstract over concrete implementations, allowing different backends
//! to provide the same information to RPC endpoints.

/// Provider trait for topology and network status information.
///
/// RPC services depend on this trait rather than concrete topology types.
/// This allows different topology implementations (Kademlia, mock, etc.)
/// to be used interchangeably.
///
/// # Implementors
///
/// - `KademliaTopology` - Production Kademlia-based topology
/// - Mock implementations for testing
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait TopologyProvider: Send + Sync + 'static {
    /// Get the node's overlay address as a hex-encoded string.
    fn overlay_address(&self) -> String;

    /// Get the current Kademlia depth.
    ///
    /// Depth indicates how "deep" into the address space we're responsible for.
    fn depth(&self) -> u8;

    /// Get the count of currently connected peers.
    fn connected_peers_count(&self) -> usize;

    /// Get the count of known (discovered but not necessarily connected) peers.
    fn known_peers_count(&self) -> usize;

    /// Get the count of pending connection attempts.
    fn pending_connections_count(&self) -> usize;

    /// Get bin sizes for each proximity order (0-31).
    ///
    /// Returns a vector of `(connected, known)` tuples, one per bin.
    fn bin_sizes(&self) -> Vec<(usize, usize)>;

    /// Get connected peer overlay addresses in a specific bin.
    ///
    /// Returns hex-encoded overlay addresses.
    fn connected_peers_in_bin(&self, po: u8) -> Vec<String>;
}

// Future providers can be added here:
//
// /// Provider trait for accounting/incentive information.
// pub trait AccountingProvider: Send + Sync + 'static {
//     fn peer_balance(&self, peer: &OverlayAddress) -> i64;
//     fn total_sent(&self) -> u64;
//     fn total_received(&self) -> u64;
// }
//
// /// Provider trait for storage information.
// pub trait StorageProvider: Send + Sync + 'static {
//     fn capacity(&self) -> u64;
//     fn used(&self) -> u64;
//     fn chunk_count(&self) -> u64;
// }
