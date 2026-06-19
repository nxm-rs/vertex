//! The committed sampling depth and the neighbourhood it defines.

use core::fmt;

use nectar_primitives::{Bin, ChunkAddress, SwarmAddress};

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
}
