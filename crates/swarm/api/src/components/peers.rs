//! Peer resolution and score persistence traits.

use vertex_swarm_primitives::OverlayAddress;

/// Resolve an overlay address to a previously verified peer.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPeerResolver: Send + Sync + 'static {
    /// The peer type returned by resolution.
    type Peer: Clone + Send + Sync + 'static;

    /// Look up a peer by overlay address.
    fn get_swarm_peer(&self, overlay: &OverlayAddress) -> Option<Self::Peer>;
}

/// Auxiliary peer score persistence (scores, ban info).
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmScoreStore: Send + Sync {
    /// Score type stored per peer.
    type Score;
    /// Error type for store operations.
    type Error: std::error::Error + Send + Sync;

    /// Load a peer's score.
    fn get_score(&self, overlay: &OverlayAddress) -> Result<Option<Self::Score>, Self::Error>;
    /// Persist a batch of scores.
    fn save_score_batch(&self, scores: &[(OverlayAddress, Self::Score)])
    -> Result<(), Self::Error>;
    /// Load banned overlay addresses (startup use).
    fn load_banned_overlays(&self) -> Result<Vec<OverlayAddress>, Self::Error> {
        Ok(Vec::new())
    }
}
