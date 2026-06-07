//! Core primitive types for Ethereum Swarm nodes.
//!
//! This crate re-exports canonical Swarm primitives from `nectar-primitives`
//! and adds vertex-specific node configuration types.
//!
//! # Proximity types: keep them distinct
//!
//! Three types share the `0..=MAX_PO` (`u8`) range but mean different things.
//! Conflating them (the bee `po: u8` habit) is the historical bug-surface, so
//! the type system keeps them apart. Use the right one and the single named
//! bridges between them:
//!
//! - [`ProximityOrder`]: the symmetric XOR-distance **metric** between two
//!   addresses (`addr.proximity(other)`). Use it directly only to rank by
//!   closeness to an arbitrary target (e.g. a chunk). It is NOT a table index.
//! - [`Bin`]: the **index** of a peer's slot in the local node's routing table.
//!   The only `ProximityOrder -> Bin` bridge is `Bin::from` (relative to the
//!   local overlay, capped at `max_po`). It keys per-bin storage and iteration.
//!   It is NOT a free metric between arbitrary addresses.
//! - [`NeighborhoodDepth`]: the distinguished **boundary** bin. Bins it
//!   [`contains`](NeighborhoodDepth::contains) are the neighborhood (area of
//!   responsibility); shallower bins are balanced. It is deliberately NOT
//!   comparable with `Bin` - relate them only through `depth.contains(bin)` or
//!   `depth.bin()`, so an accidental `bin >= depth` does not compile.
//!
//! Enumerate the bin space only through [`all_bins`], [`balanced_bins`], and
//! [`neighborhood_bins`] (the sole places a `Bin` is built from a raw index).
//! Extract the raw `u8` with `.get()` only at edges (logs, metrics, the wire).

#![cfg_attr(not(feature = "std"), no_std)]

mod validated;

pub use validated::{ValidatedChunk, ValidationError};

// Re-export canonical Swarm primitives from nectar. See the crate-level docs
// for the ProximityOrder / Bin / NeighborhoodDepth distinction.
pub use nectar_primitives::{Bin, NetworkId, Nonce, ProximityOrder, Timestamp, compute_overlay};

use core::fmt;

use nectar_primitives::SwarmAddress;

/// The neighborhood-depth boundary: bins at or beyond `depth` are the
/// neighborhood (the node's area of responsibility), shallower bins are
/// balanced.
///
/// A distinguished [`Bin`] in a specific *role*, kept distinct from an
/// arbitrary `Bin` so the two cannot be conflated. It is deliberately **not**
/// comparable with `Bin`: relate them only through [`contains`](Self::contains)
/// (membership) or [`bin`](Self::bin) (the boundary as an index). `Ord` among
/// depths is provided (deeper depth is greater) for `max`/`>` on depths.
///
/// nectar owns the depth *math* (`recompute_neighborhood_depth`); this wrapper
/// is the routing-layer type, per that function's contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NeighborhoodDepth(Bin);

impl NeighborhoodDepth {
    /// The shallowest depth (every bin is in the neighborhood).
    pub const ZERO: Self = Self(Bin::ZERO);

    /// Wrap a [`Bin`] as a depth. The only `Bin -> NeighborhoodDepth` bridge;
    /// use it at the depth computation, not to coerce arbitrary bins.
    #[inline]
    #[must_use]
    pub const fn new(bin: Bin) -> Self {
        Self(bin)
    }

    /// The boundary as a [`Bin`]. Use when you genuinely need the depth as an
    /// index.
    #[inline]
    #[must_use]
    pub const fn bin(self) -> Bin {
        self.0
    }

    /// The raw boundary index. For edges only (metric labels, wire, logs).
    #[inline]
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0.get()
    }

    /// Whether `bin` is inside the neighborhood (`bin >= depth`).
    ///
    /// O(1): a single `u8` comparison, no iteration or allocation despite the
    /// set-like name. Same cost as the raw `bin >= depth` it replaces.
    #[inline]
    #[must_use]
    pub fn contains(self, bin: Bin) -> bool {
        bin >= self.0
    }
}

impl fmt::Display for NeighborhoodDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "depth={}", self.0.get())
    }
}

/// Iterate every bin `0..=max`, ascending. Double-ended, so `.rev()` walks
/// deepest-first. This and the two depth-relative iterators below are the only
/// places a `Bin` is constructed from a raw index.
pub fn all_bins(max: Bin) -> impl DoubleEndedIterator<Item = Bin> + Clone {
    (0..=max.get()).map(|po| Bin::new(po).unwrap_or(Bin::MAX))
}

/// Iterate the balanced bins `0..depth` (shallower than the neighborhood),
/// ascending. Empty when `depth` is [`NeighborhoodDepth::ZERO`].
pub fn balanced_bins(depth: NeighborhoodDepth) -> impl DoubleEndedIterator<Item = Bin> + Clone {
    (0..depth.get()).map(|po| Bin::new(po).unwrap_or(Bin::MAX))
}

/// Iterate the neighborhood bins `depth..=max` (at or beyond the boundary),
/// ascending.
pub fn neighborhood_bins(
    depth: NeighborhoodDepth,
    max: Bin,
) -> impl DoubleEndedIterator<Item = Bin> + Clone {
    (depth.get()..=max.get()).map(|po| Bin::new(po).unwrap_or(Bin::MAX))
}

/// Overlay address for Swarm routing and peer identification.
pub type OverlayAddress = SwarmAddress;

/// Swarm node type determining capabilities and protocols.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    Hash,
    strum::Display,
    strum::FromRepr,
    strum::IntoStaticStr,
)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[strum(serialize_all = "lowercase")]
#[repr(u8)]
pub enum SwarmNodeType {
    /// Topology only: handshake, hive, ping.
    Bootnode = 0,
    /// Read + write: retrieval, pushsync, pricing.
    #[default]
    Client = 1,
    /// Storage + staking: pullsync, local storage.
    Storer = 2,
}

impl SwarmNodeType {
    pub fn requires_pricing(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    pub fn requires_accounting(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    pub fn requires_retrieval(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    pub fn requires_pushsync(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    pub fn requires_pullsync(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }

    pub fn requires_storage(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }

    pub fn supports_staking(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }

    /// Whether this node type requires a persistent (keystore-backed) signing
    /// key. Bootnodes need a stable overlay address across restarts so peers
    /// can reach them via their well-known overlay. Storers need a stable key
    /// for staking and chunk reservation.
    pub fn requires_persistent_identity(&self) -> bool {
        matches!(self, SwarmNodeType::Bootnode | SwarmNodeType::Storer)
    }

    /// Whether this node type requires a persistent nonce. Combined with a
    /// persistent signing key, this fixes the overlay address across restarts.
    pub fn requires_persistent_nonce(&self) -> bool {
        matches!(self, SwarmNodeType::Bootnode | SwarmNodeType::Storer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(n: u8) -> Bin {
        Bin::new(n).expect("valid bin")
    }

    #[test]
    fn neighborhood_depth_contains_at_or_beyond() {
        let depth = NeighborhoodDepth::new(b(5));
        // Shallower bins are balanced (not in the neighborhood).
        assert!(!depth.contains(b(0)));
        assert!(!depth.contains(b(4)));
        // The boundary and deeper bins are in the neighborhood.
        assert!(depth.contains(b(5)));
        assert!(depth.contains(b(6)));
        assert!(depth.contains(Bin::MAX));
    }

    #[test]
    fn neighborhood_depth_zero_contains_every_bin() {
        let depth = NeighborhoodDepth::ZERO;
        assert!(depth.contains(Bin::ZERO));
        assert!(depth.contains(Bin::MAX));
        assert_eq!(depth.get(), 0);
        assert_eq!(depth.bin(), Bin::ZERO);
    }

    #[test]
    fn neighborhood_depth_ord_is_by_boundary() {
        assert!(NeighborhoodDepth::new(b(7)) > NeighborhoodDepth::new(b(3)));
        assert_eq!(
            NeighborhoodDepth::new(b(7)).max(NeighborhoodDepth::new(b(3))),
            NeighborhoodDepth::new(b(7))
        );
    }

    #[test]
    fn bin_iterators_partition_the_table() {
        let depth = NeighborhoodDepth::new(b(3));
        let max = b(5);

        let balanced: Vec<u8> = balanced_bins(depth).map(Bin::get).collect();
        let neighborhood: Vec<u8> = neighborhood_bins(depth, max).map(Bin::get).collect();
        let all: Vec<u8> = all_bins(max).map(Bin::get).collect();

        assert_eq!(balanced, vec![0, 1, 2]);
        assert_eq!(neighborhood, vec![3, 4, 5]);
        assert_eq!(all, vec![0, 1, 2, 3, 4, 5]);

        // Balanced + neighborhood exactly cover the table, no overlap.
        let mut union = balanced;
        union.extend(neighborhood);
        assert_eq!(union, all);

        // Double-ended: neighborhood walks deepest-first reversed.
        let rev: Vec<u8> = neighborhood_bins(depth, max).rev().map(Bin::get).collect();
        assert_eq!(rev, vec![5, 4, 3]);
    }

    #[test]
    fn balanced_bins_empty_at_zero_depth() {
        assert_eq!(balanced_bins(NeighborhoodDepth::ZERO).count(), 0);
    }

    #[test]
    fn bootnode_requires_persistent_identity_and_nonce() {
        // Bootnode overlay is a contract: peers reach it via a well-known
        // overlay address derived from the keystore key and nonce. Both must
        // survive restart.
        assert!(SwarmNodeType::Bootnode.requires_persistent_identity());
        assert!(SwarmNodeType::Bootnode.requires_persistent_nonce());
    }

    #[test]
    fn storer_requires_persistent_identity_and_nonce() {
        assert!(SwarmNodeType::Storer.requires_persistent_identity());
        assert!(SwarmNodeType::Storer.requires_persistent_nonce());
    }

    #[test]
    fn client_does_not_require_persistence() {
        assert!(!SwarmNodeType::Client.requires_persistent_identity());
        assert!(!SwarmNodeType::Client.requires_persistent_nonce());
    }

    #[test]
    fn bootnode_excludes_client_protocols() {
        let t = SwarmNodeType::Bootnode;
        assert!(!t.requires_pricing());
        assert!(!t.requires_accounting());
        assert!(!t.requires_retrieval());
        assert!(!t.requires_pushsync());
        assert!(!t.requires_pullsync());
        assert!(!t.requires_storage());
        assert!(!t.supports_staking());
    }
}

/// Bandwidth accounting mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, strum::Display, strum::FromRepr)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[strum(serialize_all = "lowercase")]
#[repr(u8)]
pub enum BandwidthMode {
    /// No bandwidth accounting (dev/testing only).
    None = 0,
    /// Soft accounting without real payments (default).
    #[default]
    Pseudosettle = 1,
    /// Real payment channels with chequebook.
    Swap = 2,
    /// Both pseudosettle and SWAP.
    Both = 3,
}

impl BandwidthMode {
    pub fn pseudosettle_enabled(self) -> bool {
        matches!(self, BandwidthMode::Pseudosettle | BandwidthMode::Both)
    }

    pub fn swap_enabled(self) -> bool {
        matches!(self, BandwidthMode::Swap | BandwidthMode::Both)
    }

    pub fn is_enabled(self) -> bool {
        !matches!(self, BandwidthMode::None)
    }
}
