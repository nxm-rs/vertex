//! Bandwidth incentives - per-peer accounting without mutex contention.
//!
//! # Design
//!
//! The bandwidth system uses a two-level design to avoid lock contention:
//!
//! 1. [`BandwidthAccounting`] - Factory that creates per-peer handles
//! 2. [`PeerBandwidth`] - Per-peer handle with lock-free operations
//!
//! When a connection is established, call `accounting.for_peer(overlay_addr)` to get
//! a [`PeerBandwidth`] handle. This handle is `Clone` and can be shared by all
//! protocols on that connection (retrieval, pushsync, pricing, swap).
//!
//! The `record()` operation uses atomic counters, so multiple protocols can
//! record bandwidth concurrently without any locking.
//!
//! # Overlay Addresses
//!
//! All accounting uses [`OverlayAddress`] (32-byte Swarm address) for peer
//! identification, not libp2p `PeerId`. This is because:
//!
//! - Accounting is tied to the Swarm identity (overlay), not the connection (underlay)
//! - A peer may reconnect with a different underlay but same overlay
//! - Settlement (SWAP cheques) is based on overlay identity
//!
//! # Example
//!
//! ```ignore
//! // Connection established - use peer's overlay address
//! let peer_accounting = bandwidth.for_peer(peer_overlay);
//!
//! // Clone for each protocol stream
//! let retrieval_accounting = peer_accounting.clone();
//! let pushsync_accounting = peer_accounting.clone();
//!
//! // Both can record concurrently - no contention
//! retrieval_accounting.record(1024, Direction::Download);
//! pushsync_accounting.record(4096, Direction::Upload);
//! ```

use alloc::vec::Vec;
use async_trait::async_trait;
use vertex_primitives::OverlayAddress;

use crate::SwarmResult;

/// Direction of bandwidth usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Uploading data (sending to peer)
    Upload,
    /// Downloading data (receiving from peer)
    Download,
}

// ============================================================================
// Per-Peer Bandwidth Handle
// ============================================================================

/// Per-peer bandwidth accounting handle.
///
/// This is the handle used by protocol streams. It must be:
/// - `Clone` - shared across protocols on the same connection
/// - `Send + Sync` - used from async tasks
/// - Lock-free for `record()` - uses atomics internally
///
/// # Implementation Requirements
///
/// - `record()` MUST be lock-free (use `AtomicI64` or similar)
/// - `allow()` should be fast (may read atomics)
/// - `settle()` may take locks or do I/O (it's async)
#[async_trait]
pub trait PeerBandwidth: Clone + Send + Sync {
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

// ============================================================================
// Bandwidth Accounting Factory
// ============================================================================

/// Factory for creating per-peer bandwidth accounting handles.
///
/// Implementations manage the set of peer accounts and create handles
/// when connections are established.
///
/// # Lifecycle
///
/// 1. Connection established → `for_peer(peer_id)` creates/returns handle
/// 2. Protocols clone the handle and use it for bandwidth tracking
/// 3. Connection closed → implementation may clean up or keep for reconnect
pub trait BandwidthAccounting: Clone + Send + Sync {
    /// The per-peer accounting handle type.
    type Peer: PeerBandwidth;

    /// Get or create a bandwidth accounting handle for a peer.
    ///
    /// If accounting already exists for this peer, returns a handle to
    /// the existing account. Otherwise creates a new one.
    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer;

    /// List all peers with active accounting.
    fn peers(&self) -> Vec<OverlayAddress>;

    /// Remove accounting for a peer (e.g., after disconnect + timeout).
    fn remove_peer(&self, peer: &OverlayAddress);
}

// ============================================================================
// No-op Implementations
// ============================================================================

/// No-op bandwidth accounting (always allows, never settles).
///
/// Use this for testing or private networks without bandwidth accounting.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoBandwidthIncentives;

/// No-op per-peer bandwidth handle.
#[derive(Debug, Clone)]
pub struct NoPeerBandwidth {
    peer: OverlayAddress,
}

#[async_trait]
impl PeerBandwidth for NoPeerBandwidth {
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

impl BandwidthAccounting for NoBandwidthIncentives {
    type Peer = NoPeerBandwidth;

    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
        NoPeerBandwidth { peer }
    }

    fn peers(&self) -> Vec<OverlayAddress> {
        Vec::new()
    }

    fn remove_peer(&self, _peer: &OverlayAddress) {}
}
