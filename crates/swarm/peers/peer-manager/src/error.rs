//! Peer manager error types.

use vertex_net_peer_store::StoreError;

/// Errors that can occur when creating or operating the peer manager.
#[derive(Debug, thiserror::Error)]
pub enum PeerManagerError {
    /// Peer store operation failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}
