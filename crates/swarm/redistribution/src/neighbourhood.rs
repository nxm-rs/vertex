//! The committed sampling depth and the neighbourhood it defines.

use core::fmt;

use nectar_primitives::{Bin, ChunkAddress, SwarmAddress};
use vertex_swarm_api::StorageRadius;

/// The committed neighbourhood depth for a redistribution round: the boundary
/// at which a storer's reserve is sampled.
///
/// Chunks whose proximity to the round anchor meets this depth are the node's
/// committed sample neighbourhood. It is a distinguished [`Bin`] in a
/// redistribution-specific role. This is intentionally a distinct type from
/// vertex's routing [`NeighborhoodDepth`][nd]: that type is the local
/// connectivity boundary (supply-side, local-only), whereas this is the
/// per-round, on-chain-derived reserve-commitment depth. The bytes are a plain
/// `u8 >= u8` compare either way; the separate type keeps the roles from being
/// conflated.
///
/// [nd]: vertex_swarm_api::NeighborhoodDepth
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommittedDepth(Bin);

impl CommittedDepth {
    /// The shallowest depth (every address is in the neighbourhood).
    pub const ZERO: Self = Self(Bin::ZERO);

    /// Wrap a [`Bin`] as a committed depth.
    #[inline]
    #[must_use]
    pub const fn new(bin: Bin) -> Self {
        Self(bin)
    }

    /// The boundary as a [`Bin`].
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
    ///
    /// O(1): a single `u8` comparison, no iteration or allocation despite the
    /// set-like name.
    #[inline]
    #[must_use]
    pub fn contains(self, bin: Bin) -> bool {
        bin >= self.0
    }

    /// Derive the consensus-committed depth from the reserve's
    /// [`StorageRadius`] and the node's [`CapacityDoubling`] addend.
    ///
    /// This is the network-observable output the redistribution agent commits
    /// on chain: `committedDepth = storage_radius + capacity_doubling`. The
    /// storage radius is the reserve's own size-driven responsibility boundary
    /// (see the storer's [radius controller][rc]); the capacity-doubling addend
    /// lets an over-provisioned node sample a deeper neighbourhood than its bare
    /// radius would imply, without changing what it stores.
    ///
    /// The addend is applied with a *saturating* `u8` add and then clamped to
    /// the [`Bin`] range (`0..=MAX_PO`): the sum pins at [`Bin::MAX`] rather than
    /// wrapping or panicking. With the addend bounded at
    /// [`CapacityDoubling::MAX`] (`1`) the ceiling is reachable only from a radius
    /// already at the deepest bin (`MAX_PO + 1`), so the clamp is defensive
    /// belt-and-braces rather than a routine path, but it keeps the derivation
    /// total (no fallible path), which is what callers on the commit hot-path
    /// require. Because both inputs are byte-exact across nodes, so is the
    /// output, which is the consensus property a redistribution round relies on.
    ///
    /// [rc]: https://docs.rs/vertex-swarm-storer (the `radius` module)
    #[inline]
    #[must_use]
    pub fn from_radius(radius: StorageRadius, doubling: CapacityDoubling) -> Self {
        let raw = radius.get().saturating_add(doubling.get());
        // Clamp into the bin range; `Bin::try_from` only fails for `> MAX_PO`,
        // in which case the deepest bin is the correct ceiling.
        Self(Bin::try_from(raw).unwrap_or(Bin::MAX))
    }
}

/// The capacity-doubling addend a node adds to its [`StorageRadius`] to obtain
/// the [`CommittedDepth`] it samples and commits on chain.
///
/// A node with spare reserve capacity may commit to sampling a neighbourhood
/// `capacity_doubling` bins deeper than its storage radius alone would define,
/// effectively committing to hold the equivalent of `2^doubling` times the
/// baseline reserve. It is a node-configured constant for a round (read from
/// configuration, not derived from the reserve), distinct from the radius,
/// which the reserve derives from its own occupancy.
///
/// Range is `0..=`[`MAX`](Self::MAX), where `MAX` is `1`: a zero addend means the
/// committed depth equals the storage radius, and `1` is the deepest doubling the
/// protocol admits (one bin deeper, twice the baseline reserve). The bound is the
/// network's, not a representational one: a redistribution round will only ever
/// observe a committed depth derived from a doubling in `0..=1`, so a larger
/// addend is a configuration error that would put the node out of consensus with
/// the rest of the network rather than merely saturating against the bin
/// ceiling. The constructor rejects it so the misconfiguration surfaces at ingest
/// rather than producing an out-of-consensus committed depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CapacityDoubling(u8);

impl CapacityDoubling {
    /// No doubling: the committed depth equals the storage radius.
    pub const ZERO: Self = Self(0);

    /// The maximum doubling the protocol admits.
    ///
    /// The network caps the per-node capacity doubling at one bin: a node may
    /// commit to twice the baseline reserve, no more. A larger value is not a
    /// representational overflow (it stays well within `u8` and the bin range)
    /// but an out-of-consensus configuration the network would never produce, so
    /// the ingress rejects it.
    pub const MAX: Self = Self(1);

    /// The raw addend. For edges only (config display, metrics, the wire).
    #[inline]
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// The error a [`CapacityDoubling`] ingress raises for an addend above the
/// protocol-permitted maximum ([`CapacityDoubling::MAX`]).
///
/// Distinct from a bin-range error: the value is a valid `u8` and a valid bin,
/// it is the *consensus* bound it violates, so it carries the offending value
/// and the permitted maximum for a configuration diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("capacity doubling {value} exceeds the maximum permitted doubling of {max}")]
pub struct CapacityDoublingError {
    /// The rejected addend.
    pub value: u8,
    /// The permitted maximum ([`CapacityDoubling::MAX`]).
    pub max: u8,
}

/// The sole `u8` ingress for a capacity-doubling addend (typically the node's
/// configuration). Fallible because the network caps the doubling at
/// [`CapacityDoubling::MAX`]; an addend above it is a configuration error that
/// would commit an out-of-consensus depth, not a value to silently clamp.
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

/// The sole `u8` ingress for a committed depth: the on-chain boundary as read
/// from the redistribution contract. Fallible because a raw `u8` may exceed the
/// [`Bin`] range (`> MAX_PO`); the in-range cases are exactly the valid depths.
impl TryFrom<u8> for CommittedDepth {
    type Error = nectar_primitives::BinError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Bin::try_from(value).map(Self)
    }
}

/// The deterministic neighbourhood for `anchor` at the given committed `depth`.
///
/// Returns the subset of `addrs` a node is responsible for, i.e. those whose
/// proximity order to `anchor` is at least `depth`. A [`CommittedDepth::ZERO`]
/// depth admits every address.
///
/// This is a pure membership filter and imposes **no** ordering: the protocol
/// never orders the neighbourhood by distance, it streams the depth-filtered
/// chunks and orders the *sample* by transformed address (see
/// [`reserve_sample`](crate::reserve_sample)). Imposing an extra distance sort
/// here would be dead work at best and a conformance hazard at worst. Callers
/// that need a sample feed the result (in any iteration order) into
/// [`reserve_sample`](crate::reserve_sample), whose output order is fixed by the
/// transformed addresses and therefore independent of input order.
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
        // The function does not sort; it is a pure depth filter.
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
        // The network-observable consensus formula:
        // committedDepth = storage_radius + capacity_doubling.
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
        // The only way to reach the ceiling now that the addend is capped at 1
        // is a radius already at the deepest bin: radius MAX_PO + doubling 1
        // would be MAX_PO + 1, which saturating-adds then clamps to the deepest
        // bin rather than panicking or wrapping.
        let d = CommittedDepth::from_radius(radius(MAX_PO), doubling(1));
        assert_eq!(d.get(), MAX_PO, "saturating add then clamp to MAX_PO");
        assert_eq!(d.bin(), Bin::MAX);

        let edge = CommittedDepth::from_radius(radius(MAX_PO), doubling(0));
        assert_eq!(edge.get(), MAX_PO, "radius at the ceiling stays at MAX_PO");
    }

    #[test]
    fn capacity_doubling_rejects_above_maximum() {
        // The network caps the doubling at one bin (parity with bee's
        // maxAllowedDoubling = 1); 0 and 1 are admitted, 2 and above are
        // rejected as out-of-consensus configuration even though they are valid
        // bin indices.
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
