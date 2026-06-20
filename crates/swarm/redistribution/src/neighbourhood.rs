//! The committed sampling depth and the neighbourhood it defines.

use core::fmt;

use nectar_primitives::{Bin, ChunkAddress, SwarmAddress};
use vertex_swarm_api::StorageRadius;

/// The per-round, on-chain-derived depth at which a storer's reserve is sampled.
///
/// A distinct type from the routing [`NeighborhoodDepth`][nd] (the local
/// connectivity boundary) so the two roles are not conflated.
///
/// [nd]: vertex_swarm_api::NeighborhoodDepth
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommittedDepth(Bin);

impl CommittedDepth {
    /// The shallowest depth (every address is in the neighbourhood).
    pub const ZERO: Self = Self(Bin::ZERO);

    #[inline]
    #[must_use]
    pub const fn new(bin: Bin) -> Self {
        Self(bin)
    }

    #[inline]
    #[must_use]
    pub const fn bin(self) -> Bin {
        self.0
    }

    /// The raw boundary index. For edges only (logs, metrics, the wire).
    #[inline]
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0.get()
    }

    /// Whether `bin` is inside the committed neighbourhood (`bin >= depth`).
    #[inline]
    #[must_use]
    pub fn contains(self, bin: Bin) -> bool {
        bin >= self.0
    }

    /// Derive the consensus-committed depth: `storage_radius + capacity_doubling`.
    ///
    /// Saturating add then clamp to the [`Bin`] range pins at [`Bin::MAX`] rather
    /// than wrapping or panicking, keeping the derivation total for the commit
    /// hot-path. With both inputs byte-exact across nodes the output is too, which
    /// is the consensus property a redistribution round relies on.
    #[inline]
    #[must_use]
    pub fn from_radius(radius: StorageRadius, doubling: CapacityDoubling) -> Self {
        let raw = radius.get().saturating_add(doubling.get());
        Self(Bin::try_from(raw).unwrap_or(Bin::MAX))
    }
}

/// The addend a node adds to its [`StorageRadius`] to obtain the
/// [`CommittedDepth`] it samples and commits on chain.
///
/// A node-configured constant for a round (not derived from the reserve). Range
/// is `0..=`[`MAX`](Self::MAX): a doubling of `1` commits to twice the baseline
/// reserve, one bin deeper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CapacityDoubling(u8);

impl CapacityDoubling {
    /// No doubling: the committed depth equals the storage radius.
    pub const ZERO: Self = Self(0);

    /// The maximum doubling the protocol admits. A larger value is a valid `u8`
    /// and bin index but out-of-consensus configuration, so the ingress rejects
    /// it rather than clamping.
    pub const MAX: Self = Self(1);

    /// The raw addend. For edges only (config display, metrics, the wire).
    #[inline]
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Raised for an addend above [`CapacityDoubling::MAX`]. Carries the offending
/// value and permitted maximum for a configuration diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("capacity doubling {value} exceeds the maximum permitted doubling of {max}")]
pub struct CapacityDoublingError {
    pub value: u8,
    pub max: u8,
}

/// Fallible because an addend above [`CapacityDoubling::MAX`] is a configuration
/// error that would commit an out-of-consensus depth, not a value to clamp.
impl TryFrom<u8> for CapacityDoubling {
    type Error = CapacityDoublingError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value > Self::MAX.get() {
            Err(CapacityDoublingError {
                value,
                max: Self::MAX.get(),
            })
        } else {
            Ok(Self(value))
        }
    }
}

impl fmt::Display for CapacityDoubling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "capacity_doubling={}", self.0)
    }
}

impl fmt::Display for CommittedDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "depth={}", self.0.get())
    }
}

/// Fallible because a raw `u8` may exceed the [`Bin`] range (`> MAX_PO`).
impl TryFrom<u8> for CommittedDepth {
    type Error = nectar_primitives::BinError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Bin::try_from(value).map(Self)
    }
}

/// The subset of `addrs` whose proximity order to `anchor` is at least `depth`.
///
/// A pure membership filter imposing no ordering: the protocol orders the
/// *sample*, not the neighbourhood (see [`reserve_sample`](crate::reserve_sample),
/// whose output order is fixed by transformed addresses and independent of input
/// order). [`CommittedDepth::ZERO`] admits every address.
///
/// # Examples
///
/// ```
/// use vertex_swarm_redistribution::{CommittedDepth, canonical_neighbourhood};
/// use nectar_primitives::SwarmAddress;
/// use alloy_primitives::B256;
///
/// let anchor = SwarmAddress::zero();
/// let near = SwarmAddress::from(B256::ZERO);
/// let far = SwarmAddress::from(B256::repeat_byte(0xff));
/// let depth = CommittedDepth::try_from(1).unwrap();
/// let hood = canonical_neighbourhood(&anchor, depth, [near, far]);
/// assert_eq!(hood, vec![near]);
/// ```
#[must_use]
pub fn canonical_neighbourhood(
    anchor: &SwarmAddress,
    depth: CommittedDepth,
    addrs: impl IntoIterator<Item = ChunkAddress>,
) -> Vec<ChunkAddress> {
    addrs
        .into_iter()
        .filter(|addr| depth.contains(Bin::from(anchor.proximity(addr))))
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test assertions over known-bounds inputs"
)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use nectar_primitives::MAX_PO;

    fn addr(byte: u8) -> ChunkAddress {
        SwarmAddress::from(B256::repeat_byte(byte))
    }

    fn depth(n: u8) -> CommittedDepth {
        CommittedDepth::try_from(n).unwrap()
    }

    #[test]
    fn canonical_neighbourhood_filters_by_depth() {
        let anchor = SwarmAddress::zero();
        let near = addr(0x00);
        let far = addr(0xff);

        let hood = canonical_neighbourhood(&anchor, depth(1), [near, far]);
        assert_eq!(hood, vec![near], "depth filter must drop distant addresses");

        let all = canonical_neighbourhood(&anchor, depth(0), [near, far]);
        assert_eq!(all.len(), 2, "depth 0 admits every address");
    }

    #[test]
    fn canonical_neighbourhood_preserves_input_order() {
        let anchor = SwarmAddress::zero();
        let addrs = vec![addr(0x01), addr(0x02), addr(0x03)];
        let hood = canonical_neighbourhood(&anchor, depth(0), addrs.clone());
        assert_eq!(hood, addrs);
    }

    #[test]
    fn committed_depth_round_trips_u8() {
        assert_eq!(CommittedDepth::try_from(0).unwrap().get(), 0);
        assert_eq!(CommittedDepth::try_from(31).unwrap().get(), 31);
        assert!(CommittedDepth::try_from(32).is_err());
        assert_eq!(CommittedDepth::ZERO, CommittedDepth::try_from(0).unwrap());
    }

    fn radius(n: u8) -> StorageRadius {
        StorageRadius::new(Bin::try_from(n).unwrap())
    }

    fn doubling(n: u8) -> CapacityDoubling {
        CapacityDoubling::try_from(n).unwrap()
    }

    #[test]
    fn committed_depth_is_radius_plus_doubling() {
        assert_eq!(
            CommittedDepth::from_radius(radius(8), doubling(0)).get(),
            8,
            "zero doubling => committed depth equals storage radius"
        );
        assert_eq!(
            CommittedDepth::from_radius(radius(8), doubling(1)).get(),
            9,
            "the maximum addend (1) is added to the radius"
        );
        assert_eq!(
            CommittedDepth::from_radius(radius(0), doubling(1)).get(),
            1,
            "from a zero radius the depth is the addend alone"
        );
    }

    #[test]
    fn committed_depth_clamps_to_deepest_bin() {
        let d = CommittedDepth::from_radius(radius(MAX_PO), doubling(1));
        assert_eq!(d.get(), MAX_PO, "saturating add then clamp to MAX_PO");
        assert_eq!(d.bin(), Bin::MAX);

        let edge = CommittedDepth::from_radius(radius(MAX_PO), doubling(0));
        assert_eq!(edge.get(), MAX_PO, "radius at the ceiling stays at MAX_PO");
    }

    #[test]
    fn capacity_doubling_rejects_above_maximum() {
        assert_eq!(CapacityDoubling::try_from(0).unwrap().get(), 0);
        assert_eq!(CapacityDoubling::try_from(1).unwrap().get(), 1);
        assert_eq!(
            CapacityDoubling::try_from(1).unwrap(),
            CapacityDoubling::MAX
        );
        let err = CapacityDoubling::try_from(2).unwrap_err();
        assert_eq!(err.value, 2);
        assert_eq!(err.max, 1, "the diagnostic reports the permitted maximum");
        assert!(
            CapacityDoubling::try_from(MAX_PO).is_err(),
            "even a valid bin index above the doubling maximum is rejected"
        );
        assert_eq!(CapacityDoubling::ZERO.get(), 0);
        assert_eq!(CapacityDoubling::default(), CapacityDoubling::ZERO);
    }
}
