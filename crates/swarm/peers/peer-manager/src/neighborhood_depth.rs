//! Typed neighborhood depth.
//!
//! The neighborhood depth is the shallowest [`Bin`] that the node considers
//! "neighborhood" (bins with index `>= depth` are inside the local
//! neighborhood and receive saturated capacity treatment). It is a property
//! of the routing table as a whole, derived from per-bin connected-peer
//! counts via [`nectar_primitives::recompute_neighborhood_depth`], which
//! mirrors bee's `recalcDepth`.
//!
//! `NeighborhoodDepth` is a vertex routing-layer wrapper around nectar's
//! canonical [`Bin`]; the math itself lives in nectar.

use vertex_swarm_primitives::{Bin, NUM_BINS, recompute_neighborhood_depth};

/// Typed neighborhood depth: a single [`Bin`] identifying the shallowest
/// saturated bin in the routing table.
///
/// See bee reference `pkg/topology/kademlia/kademlia.go:896-920`
/// (`recalcDepth`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NeighborhoodDepth(Bin);

impl Default for NeighborhoodDepth {
    #[inline]
    fn default() -> Self {
        Self::ZERO
    }
}

impl NeighborhoodDepth {
    /// The shallowest possible depth (`bin = 0`).
    pub const ZERO: Self = Self(Bin::ZERO);

    /// Wrap a [`Bin`] as a `NeighborhoodDepth`.
    #[inline]
    #[must_use]
    pub const fn new(bin: Bin) -> Self {
        Self(bin)
    }

    /// The underlying [`Bin`].
    #[inline]
    #[must_use]
    pub const fn bin(self) -> Bin {
        self.0
    }

    /// Raw `u8` value.
    #[inline]
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0.get()
    }

    /// Recompute the neighborhood depth from per-bin connected-peer counts.
    ///
    /// Thin wrapper over [`nectar_primitives::recompute_neighborhood_depth`]:
    /// the algorithm itself (port of bee `kademlia.go:896-920`) lives in
    /// nectar; vertex only owns the `NeighborhoodDepth` newtype.
    ///
    /// * `connected_per_bin[i]` — count of connected peers in bin `i`. Indexed
    ///   `0..NUM_BINS` (32 entries). Each count is clamped to `u8::MAX`
    ///   before being handed to nectar; production routing tables never come
    ///   close to that ceiling per bin.
    /// * `saturation` — bee's `SaturationPeers` (a.k.a. nominal).
    /// * `low_watermark` — bee's `LowWaterMark`.
    #[must_use]
    pub fn recompute(
        connected_per_bin: &[usize; NUM_BINS],
        saturation: usize,
        low_watermark: usize,
    ) -> Self {
        let mut counts = [0u8; NUM_BINS];
        for (slot, &src) in counts.iter_mut().zip(connected_per_bin.iter()) {
            *slot = u8::try_from(src).unwrap_or(u8::MAX);
        }
        let saturation_u8 = u8::try_from(saturation).unwrap_or(u8::MAX);
        let watermark_u8 = u8::try_from(low_watermark).unwrap_or(u8::MAX);
        Self(recompute_neighborhood_depth(
            &counts,
            saturation_u8,
            watermark_u8,
        ))
    }
}

impl From<NeighborhoodDepth> for Bin {
    #[inline]
    fn from(d: NeighborhoodDepth) -> Self {
        d.0
    }
}

impl From<NeighborhoodDepth> for u8 {
    #[inline]
    fn from(d: NeighborhoodDepth) -> Self {
        d.0.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `[usize; NUM_BINS]` from a list of `(bin, count)` pairs.
    fn bins(pairs: &[(u8, usize)]) -> [usize; NUM_BINS] {
        let mut arr = [0usize; NUM_BINS];
        for &(b, c) in pairs {
            arr[b as usize] = c;
        }
        arr
    }

    #[test]
    fn empty_returns_zero() {
        let counts = [0usize; NUM_BINS];
        let d = NeighborhoodDepth::recompute(&counts, 4, 4);
        assert_eq!(d, NeighborhoodDepth::ZERO);
    }

    #[test]
    fn single_shallow_bin_above_watermark() {
        // Bin 0 has 5 peers. Saturation is 4 so candidate=0 (bin 0 already
        // saturated, bin 1 unsaturated). Watermark walk from deepest backs
        // up to bin 0 (only populated bin). Depth = 0.
        let counts = bins(&[(0, 5)]);
        let d = NeighborhoodDepth::recompute(&counts, 4, 4);
        assert_eq!(d.get(), 0);
    }

    #[test]
    fn fully_saturated_yields_max() {
        // Every bin has exactly the saturation count. Per nectar's algorithm
        // when no bin is below saturation, the depth anchors at MAX_PO via
        // the watermark tail walk.
        let counts = [4usize; NUM_BINS];
        let d = NeighborhoodDepth::recompute(&counts, 4, 4);
        assert_eq!(d.bin(), Bin::MAX);
    }

    #[test]
    fn shallow_unsaturated_caps_depth() {
        // Bin 3 unsaturated with 1 peer; deep bins all have plenty. The
        // candidate is bin 3, so depth cannot exceed bin 3.
        let mut counts = [4usize; NUM_BINS];
        counts[3] = 1;
        let d = NeighborhoodDepth::recompute(&counts, 4, 4);
        assert!(d.get() <= 3);
    }

    #[test]
    fn zero_watermark_returns_candidate() {
        // With low_watermark=0 the result is exactly the shallowest
        // unsaturated bin.
        let mut counts = [4usize; NUM_BINS];
        counts[5] = 2;
        let d = NeighborhoodDepth::recompute(&counts, 4, 0);
        assert_eq!(d.get(), 5);
    }

    #[test]
    fn conversions() {
        let d = NeighborhoodDepth::new(Bin::new(5).unwrap());
        let bin: Bin = d.into();
        assert_eq!(bin.get(), 5);
        let raw: u8 = d.into();
        assert_eq!(raw, 5);
    }

    #[test]
    fn count_overflow_is_clamped() {
        // Defensive: nectar takes u8 counts; vertex stores usize. Anything
        // above u8::MAX gets clamped, not truncated.
        let mut counts = [0usize; NUM_BINS];
        counts[0] = (u8::MAX as usize) + 100;
        let d = NeighborhoodDepth::recompute(&counts, 4, 4);
        // Bin 0 is over-saturated, so the candidate is bin 1, depth <= 1.
        assert!(d.get() <= 1);
    }
}
