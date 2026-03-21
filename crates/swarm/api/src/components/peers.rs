//! Peer resolution, registry, and store traits.
//!
//! Three complementary abstractions for per-peer state keyed by
//! [`OverlayAddress`]:
//!
//! - [`SwarmPeerRegistry`] -- in-memory get-or-create, list, remove
//! - [`SwarmPeerStore`] -- persistent load/save (scores, accounting, etc.)
//! - [`SwarmPeerResolver`] -- read-only lookup of verified peers

use std::vec::Vec;

use vertex_swarm_primitives::OverlayAddress;

/// Overlay-keyed peer registry with get-or-create, list, and remove.
///
/// Generic over the per-peer handle type. Bandwidth accounting, settlement
/// services, and other per-peer subsystems can share this interface.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPeerRegistry: Send + Sync {
    /// The per-peer handle type.
    type Peer;

    /// Get or create a handle for a peer.
    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer;

    /// List all peers with active handles.
    fn peers(&self) -> Vec<OverlayAddress>;

    /// Remove a peer's handle.
    fn remove_peer(&self, peer: &OverlayAddress);
}

/// Overlay-keyed persistent store for per-peer data.
///
/// Captures the shared load/save pattern used by score stores,
/// accounting stores, and similar subsystems.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPeerStore: Send + Sync {
    /// The per-peer value type (scores, accounting state, etc.).
    type Value;
    /// Error type for store operations.
    type Error: std::error::Error + Send + Sync;

    /// Load a single peer's value.
    fn load(&self, peer: &OverlayAddress) -> Result<Option<Self::Value>, Self::Error>;

    /// Persist a batch of per-peer values in a single transaction.
    fn store_batch(&self, entries: &[(OverlayAddress, Self::Value)]) -> Result<(), Self::Error>;
}

/// Score persistence with ban tracking.
///
/// Extends [`SwarmPeerStore`] with banned-peer queries.
pub trait SwarmScoreStore: SwarmPeerStore {
    /// Load banned overlay addresses (startup use).
    fn load_banned_overlays(&self) -> Result<Vec<OverlayAddress>, Self::Error> {
        Ok(Vec::new())
    }
}

/// Resolve an overlay address to a previously verified peer.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPeerResolver: Send + Sync + 'static {
    /// The peer type returned by resolution.
    type Peer: Clone + Send + Sync + 'static;

    /// Look up a peer by overlay address.
    fn get_swarm_peer(&self, overlay: &OverlayAddress) -> Option<Self::Peer>;
}
