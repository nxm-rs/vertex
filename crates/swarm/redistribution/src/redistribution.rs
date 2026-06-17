//! Redistribution-game primitives.
//!
//! Pure compute helpers underpinning the storage-incentives redistribution
//! game: deterministic, uniform sampling over a neighbourhood and a proof of
//! entitlement for an individual chunk. Everything here is a pure function of
//! its inputs, with no I/O, no storage, no node, and no async machinery, so it
//! produces identical results on every participating node given identical
//! inputs. That determinism is what lets independent nodes converge on the same
//! sample and the same entitlement witnesses.
//!
//! These are the building blocks for vertex's storer `RedistributionStore`:
//!
//! - [`canonical_neighbourhood`] derives the deterministic, distance-ordered
//!   set of chunk addresses a node is responsible for at a given depth.
//! - [`sample`] draws a deterministic, seed-driven uniform sample of indices
//!   into a candidate set, so all nodes select the same chunks for a given
//!   commitment round.
//! - [`Entitlement`] packages a BMT inclusion proof for a sampled chunk, which
//!   a node presents as proof of entitlement during a redistribution round.
//!
//! # Determinism
//!
//! The neighbourhood is ordered by the **full** 256-bit XOR distance via
//! [`SwarmAddress::distance_cmp`], not by proximity order. Proximity order (and
//! in particular [`SwarmAddress::extended_proximity`]) caps at 36 bits, so two
//! addresses sharing a 36-bit prefix would compare equal and their relative
//! order would be left to the sort's tie-breaking. That would make the ordering
//! non-deterministic across peers holding different subsets, breaking the
//! cross-peer agreement the redistribution game relies on. Comparing the full
//! distance keeps the order total and reproducible.

use alloy_primitives::{B256, Keccak256};

use nectar_primitives::bmt::Prover;
use nectar_primitives::error::Result;
use nectar_primitives::{ChunkAddress, DefaultHasher, Proof, SwarmAddress};

/// The deterministic neighbourhood for `anchor` at the given `depth`.
///
/// Filters `addrs` to those within `depth` of `anchor` (i.e. those whose
/// proximity order to `anchor` is at least `depth`) and returns them sorted by
/// ascending full 256-bit XOR distance from `anchor` (closest first).
///
/// Ordering uses [`SwarmAddress::distance_cmp`] over the complete 256-bit
/// distance rather than proximity order. A proximity-order comparison caps at
/// [`EXTENDED_PO`](nectar_primitives::EXTENDED_PO) (36 bits) and would collapse
/// addresses that share a 36-bit prefix into an
/// equal-and-thus-unstably-ordered class, breaking cross-peer determinism. The
/// full-distance comparison yields a total order that every node reproduces
/// identically.
///
/// The membership test is `anchor.proximity(addr) >= depth`. A `depth` of `0`
/// admits every address; a `depth` greater than the address length simply
/// yields the empty set for distinct addresses.
///
/// # Examples
///
/// ```
/// use vertex_swarm_redistribution::canonical_neighbourhood;
/// use nectar_primitives::SwarmAddress;
/// use alloy_primitives::B256;
///
/// let anchor = SwarmAddress::zero();
/// let near = SwarmAddress::from(B256::ZERO); // proximity 256 (capped) to anchor
/// let far = SwarmAddress::from(B256::repeat_byte(0xff));
/// let hood = canonical_neighbourhood(&anchor, 1, [near, far]);
/// assert_eq!(hood, vec![near]);
/// ```
#[must_use]
pub fn canonical_neighbourhood(
    anchor: &SwarmAddress,
    depth: u8,
    addrs: impl IntoIterator<Item = ChunkAddress>,
) -> Vec<ChunkAddress> {
    let mut hood: Vec<ChunkAddress> = addrs
        .into_iter()
        .filter(|addr| u8::from(anchor.proximity(addr)) >= depth)
        .collect();

    // Sort by ascending full 256-bit XOR distance from the anchor (closest
    // first). `distance_cmp(a, b)` returns `Greater` when `a` is closer, so
    // invert it to put the closest address first.
    hood.sort_by(|a, b| anchor.distance_cmp(a, b).reverse());
    hood
}

/// Deterministically select `n` indices into `candidates` from `seed`.
///
/// Returns indices in `0..candidates.len()`, identical on every node for the
/// same `seed` and `candidates`. Selection is sampling **without replacement**:
/// each returned index is distinct. If `n` is at least `candidates.len()` the
/// result is a deterministic permutation of every index; if `candidates` is
/// empty the result is empty.
///
/// The draw walks a Keccak-based stream keyed by `seed`: round `i` hashes the
/// seed with the little-endian round counter to obtain a 256-bit value, which
/// is reduced modulo the number of remaining candidates to pick a position in
/// the as-yet-unselected set. Reducing a 256-bit value into a small range
/// introduces only negligible modulo bias, so the sample is effectively uniform
/// while remaining fully reproducible.
///
/// # Examples
///
/// ```
/// use vertex_swarm_redistribution::sample;
/// use alloy_primitives::B256;
///
/// let seed = B256::repeat_byte(0x42);
/// let candidates: Vec<_> = (0..10u8).map(B256::repeat_byte).collect();
/// let a = sample(seed, &candidates, 3);
/// let b = sample(seed, &candidates, 3);
/// assert_eq!(a, b); // deterministic
/// assert_eq!(a.len(), 3);
/// ```
#[must_use]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "fixed 16-byte digest tail and an in-bounds pool index"
)]
pub fn sample<T>(seed: B256, candidates: &[T], n: usize) -> Vec<usize> {
    let len = candidates.len();
    let take = n.min(len);
    if take == 0 {
        return Vec::new();
    }

    // Partial Fisher-Yates over an index permutation, with the swap target at
    // each step drawn deterministically from the Keccak stream keyed by `seed`.
    let mut pool: Vec<usize> = (0..len).collect();
    let mut out = Vec::with_capacity(take);

    for (round, remaining) in (0..take).zip((1..=len).rev()) {
        let mut hasher = Keccak256::new();
        hasher.update(seed.as_slice());
        hasher.update((round as u64).to_le_bytes());
        let digest = hasher.finalize();

        // Reduce the 256-bit digest into `0..remaining`. Modulo bias is
        // negligible for the candidate-set sizes used here.
        let value = u128::from_be_bytes(digest[16..32].try_into().expect("16-byte slice"));
        let pick = (value % remaining as u128) as usize;

        out.push(pool[pick]);
        // Remove the chosen index by swapping in the current tail, so the next
        // round draws from the remaining unselected positions.
        pool.swap_remove(pick);
    }

    out
}

/// A proof that a node holds a specific sampled chunk.
///
/// An entitlement binds a [`ChunkAddress`], a BMT inclusion [`Proof`] for one
/// of its segments, and the redistribution `seed` that selected it. It is the
/// witness a node presents during a redistribution round to demonstrate that it
/// actually stores the chunk content backing a sampled address.
#[derive(Clone, Debug)]
pub struct Entitlement {
    /// The address of the chunk this entitlement is for.
    pub address: ChunkAddress,
    /// The BMT inclusion proof for the selected segment of the chunk.
    pub proof: Proof,
    /// The redistribution seed that selected this chunk.
    pub seed: B256,
}

impl Entitlement {
    /// Build an entitlement for `segment_index` of `chunk_bytes`.
    ///
    /// Computes a BMT inclusion proof for the given segment using the existing
    /// [`Prover`] and packages it with `address` and `seed`. `chunk_bytes` is
    /// the chunk's BMT body (its span is set to the body length, matching the
    /// convention used throughout the BMT layer), and `segment_index` selects
    /// which 32-byte segment to prove.
    ///
    /// # Errors
    ///
    /// Returns an error if `segment_index` is out of bounds for the BMT, as
    /// surfaced by [`Prover::generate_proof`].
    pub fn build(
        seed: B256,
        address: ChunkAddress,
        chunk_bytes: &[u8],
        segment_index: usize,
    ) -> Result<Self> {
        let mut hasher = DefaultHasher::new();
        hasher.set_span(chunk_bytes.len() as u64);
        hasher.update(chunk_bytes);
        let proof = hasher.generate_proof(chunk_bytes, segment_index)?;

        Ok(Self {
            address,
            proof,
            seed,
        })
    }

    /// Verify this entitlement's inclusion proof against `root_hash`.
    ///
    /// Delegates to [`Proof::verify`], returning `true` when the proof's segment
    /// hashes up to `root_hash`. `root_hash` is the BMT root of the chunk body
    /// (the content-addressed component of the chunk's address).
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying proof is malformed (e.g. a wrong
    /// number of proof segments), as surfaced by [`Proof::verify`].
    pub fn verify(&self, root_hash: &[u8]) -> Result<bool> {
        self.proof.verify(root_hash)
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions over known-bounds fixtures"
)]
mod tests {
    use super::*;
    use nectar_primitives::DEFAULT_BODY_SIZE;

    fn addr(byte: u8) -> ChunkAddress {
        SwarmAddress::from(B256::repeat_byte(byte))
    }

    #[test]
    fn canonical_neighbourhood_is_deterministic() {
        let anchor = SwarmAddress::zero();
        let addrs: Vec<ChunkAddress> = (1..=20u8).map(addr).collect();

        let a = canonical_neighbourhood(&anchor, 0, addrs.clone());
        let b = canonical_neighbourhood(&anchor, 0, addrs.clone());
        assert_eq!(a, b, "same inputs must yield the same neighbourhood");

        // Independent of input ordering: a reversed input yields the same
        // canonical output (the sort is total over full distance).
        let mut reversed = addrs;
        reversed.reverse();
        let c = canonical_neighbourhood(&anchor, 0, reversed);
        assert_eq!(a, c, "ordering must not depend on input order");
    }

    #[test]
    fn canonical_neighbourhood_sorted_by_full_distance() {
        let anchor = SwarmAddress::zero();
        let addrs: Vec<ChunkAddress> = vec![addr(0x04), addr(0x01), addr(0x08), addr(0x02)];

        let hood = canonical_neighbourhood(&anchor, 0, addrs);

        // From the zero anchor, distance equals the address itself, so the
        // ascending-distance order is the numeric order of the repeated byte.
        assert_eq!(hood, vec![addr(0x01), addr(0x02), addr(0x04), addr(0x08)]);

        // Verify monotonic non-decreasing distance pairwise.
        for w in hood.windows(2) {
            let d0 = anchor.distance(&w[0]);
            let d1 = anchor.distance(&w[1]);
            assert!(
                d0 <= d1,
                "neighbourhood must be sorted by ascending distance"
            );
        }
    }

    #[test]
    fn canonical_neighbourhood_filters_by_depth() {
        let anchor = SwarmAddress::zero();
        // 0x00 shares all leading bits with the zero anchor (high proximity);
        // 0xff shares none (proximity 0).
        let near = addr(0x00);
        let far = addr(0xff);

        let hood = canonical_neighbourhood(&anchor, 1, [near, far]);
        assert_eq!(hood, vec![near], "depth filter must drop distant addresses");

        let all = canonical_neighbourhood(&anchor, 0, [near, far]);
        assert_eq!(all.len(), 2, "depth 0 admits every address");
    }

    #[test]
    fn sample_is_deterministic() {
        let seed = B256::repeat_byte(0x42);
        let candidates: Vec<ChunkAddress> = (0..32u8).map(addr).collect();

        let a = sample(seed, &candidates, 8);
        let b = sample(seed, &candidates, 8);
        assert_eq!(a, b, "same seed and candidates must yield the same indices");

        // A different seed should (overwhelmingly likely) yield a different draw.
        let other = sample(B256::repeat_byte(0x43), &candidates, 8);
        assert_ne!(a, other, "different seed should change the sample");
    }

    #[test]
    fn sample_without_replacement_and_bounds() {
        let seed = B256::repeat_byte(0x07);
        let candidates: Vec<ChunkAddress> = (0..16u8).map(addr).collect();

        let picks = sample(seed, &candidates, 10);
        assert_eq!(picks.len(), 10);
        for &i in &picks {
            assert!(i < candidates.len(), "indices stay in bounds");
        }
        let mut sorted = picks.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), picks.len(), "indices must be distinct");

        // Asking for more than available yields a full permutation.
        let all = sample(seed, &candidates, 999);
        assert_eq!(all.len(), candidates.len());
        let mut perm = all;
        perm.sort_unstable();
        assert_eq!(perm, (0..candidates.len()).collect::<Vec<_>>());

        // Empty candidate set yields no indices.
        let empty: Vec<ChunkAddress> = Vec::new();
        assert!(sample(seed, &empty, 4).is_empty());
    }

    #[test]
    fn entitlement_build_and_verify_round_trip() {
        let seed = B256::repeat_byte(0x55);
        let data = b"redistribution entitlement round trip data sample";
        let mut buf = vec![0u8; DEFAULT_BODY_SIZE];
        buf[..data.len()].copy_from_slice(data);

        // Compute the BMT root the same way `build` does internally.
        let mut hasher = DefaultHasher::new();
        hasher.set_span(buf.len() as u64);
        hasher.update(&buf);
        let root = hasher.sum();
        let address = SwarmAddress::from(root);

        for segment_index in [0usize, 1, 63, 127] {
            let ent = Entitlement::build(seed, address, &buf, segment_index)
                .expect("entitlement build should succeed");

            assert_eq!(ent.address, address);
            assert_eq!(ent.seed, seed);
            assert_eq!(ent.proof.segment_index, segment_index);

            assert!(
                ent.verify(root.as_slice())
                    .expect("verify should not error"),
                "entitlement must verify against the chunk's BMT root",
            );

            // A wrong root must not verify.
            let wrong = B256::repeat_byte(0xaa);
            assert!(
                !ent.verify(wrong.as_slice())
                    .expect("verify should not error"),
                "entitlement must reject a wrong root",
            );
        }
    }
}
