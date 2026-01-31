//! Bandwidth incentives - per-peer accounting.
//!
//! Two-level design: [`SwarmBandwidthAccounting`] creates per-peer [`SwarmPeerBandwidth`] handles.
//! Uses overlay addresses for peer identification (not libp2p `PeerId`).

use std::vec::Vec;
use vertex_swarm_primitives::OverlayAddress;

use crate::{SwarmIdentity, SwarmResult};

/// Direction of data transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Uploading data (sending to peer)
    Upload,
    /// Downloading data (receiving from peer)
    Download,
}

/// Per-peer bandwidth accounting handle.
///
/// Clone and share across protocols on the same connection.
/// `record()` is lock-free (uses atomics).
#[async_trait::async_trait]
pub trait SwarmPeerBandwidth: Clone + Send + Sync {
    /// Record bandwidth usage (lock-free).
    ///
    /// This is called frequently from protocol handlers and MUST NOT block.
    /// Use atomic operations internally.
    fn record(&self, bytes: u64, direction: Direction);

    /// Check if a transfer of `bytes` is allowed.
    ///
    /// Returns `false` if the peer owes us too much (over disconnect threshold).
    fn allow(&self, bytes: u64) -> bool;

    /// Get the current balance (positive = peer owes us).
    fn balance(&self) -> i64;

    /// Request settlement of outstanding balance.
    ///
    /// This may involve network I/O (sending cheques, etc.) so it's async.
    async fn settle(&self) -> SwarmResult<()>;

    /// Get the overlay address this handle is for.
    fn peer(&self) -> OverlayAddress;
}

/// Factory for creating per-peer bandwidth accounting handles.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmBandwidthAccounting: Send + Sync {
    /// The node identity type, providing access to overlay address, signer, etc.
    type Identity: SwarmIdentity;

    /// The per-peer accounting handle type.
    type Peer: SwarmPeerBandwidth;

    /// Get the node's identity.
    fn identity(&self) -> &Self::Identity;

    /// Get or create an availability accounting handle for a peer.
    ///
    /// If accounting already exists for this peer, returns a handle to
    /// the existing account. Otherwise creates a new one.
    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer;

    /// List all peers with active accounting.
    fn peers(&self) -> Vec<OverlayAddress>;

    /// Remove accounting for a peer (e.g., after disconnect + timeout).
    fn remove_peer(&self, peer: &OverlayAddress);
}

/// No-op bandwidth accounting (always allows, never settles).
#[derive(Debug, Clone)]
pub struct NoBandwidthIncentives<I: SwarmIdentity> {
    identity: I,
}

impl<I: SwarmIdentity> NoBandwidthIncentives<I> {
    /// Create a new no-op bandwidth accounting with the given identity.
    pub fn new(identity: I) -> Self {
        Self { identity }
    }
}

/// No-op per-peer bandwidth handle.
#[derive(Debug, Clone)]
pub struct NoPeerBandwidth {
    peer: OverlayAddress,
}

#[async_trait::async_trait]
impl SwarmPeerBandwidth for NoPeerBandwidth {
    fn record(&self, _bytes: u64, _direction: Direction) {}

    fn allow(&self, _bytes: u64) -> bool {
        true
    }

    fn balance(&self) -> i64 {
        0
    }

    async fn settle(&self) -> SwarmResult<()> {
        Ok(())
    }

    fn peer(&self) -> OverlayAddress {
        self.peer
    }
}

impl<I: SwarmIdentity> SwarmBandwidthAccounting for NoBandwidthIncentives<I> {
    type Identity = I;
    type Peer = NoPeerBandwidth;

    fn identity(&self) -> &I {
        &self.identity
    }

    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
        NoPeerBandwidth { peer }
    }

    fn peers(&self) -> Vec<OverlayAddress> {
        Vec::new()
    }

    fn remove_peer(&self, _peer: &OverlayAddress) {}
}
