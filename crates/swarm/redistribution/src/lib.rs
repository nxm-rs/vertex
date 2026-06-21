//! Swarm redistribution (storage incentives).
//!
//! Holds participation configuration ([`RedistributionArgs`], [`StorageConfig`])
//! and the consensus primitives: a deterministic reserve sample over a
//! neighbourhood and a proof of entitlement over it. The primitives are pure
//! functions of their inputs, so every participating node computes identical
//! results.
//!
//! Transformed addresses, the selected sample, and the inclusion proofs must be
//! byte-exact: the same values are re-verified on chain by the
//! `Redistribution.sol` contract, and any divergence loses or is slashed in the
//! round. Conformance is pinned by the vectors in `tests/`. The transformed
//! address that orders the sample is the nectar primitive
//! [`AnyChunk::transformed_address`](nectar_primitives::AnyChunk::transformed_address),
//! consumed here rather than re-derived.
//!
//! Building blocks:
//!
//! - [`SampleAnchor`] / [`ClaimAnchor`]: the two per-round reserve salts as
//!   distinct types so they cannot be transposed.
//! - [`CommittedDepth`] / [`canonical_neighbourhood`]: the addresses a node is
//!   responsible for at a given depth.
//! - [`CandidateFilter`] / [`RoundBatches`]: candidate-feed filters (future
//!   timestamp, below-minimum-balance, rogue/invalid stamp) applied against a
//!   round-consistent batch snapshot before sampling.
//! - [`SampleItem`] / [`reserve_sample`]: the [`SAMPLE_SIZE`] chunks with the
//!   smallest transformed addresses, each carrying the stamp its slot was won
//!   with.
//! - [`WitnessIndices`] / [`witness_indices`]: the sample slots a claim opens.
//! - [`make_inclusion_proofs`] / [`ChunkInclusionProof`]: the proof of
//!   entitlement submitted to the contract, each witness carrying its winning
//!   stamp as its single `PostageProof`.

mod anchor;
mod args;
mod config;
mod filter;
mod neighbourhood;
mod proof;
mod sample;
mod witness;

/// On-chain indexing for the Redistribution and StakeRegistry contracts, behind
/// the non-default `chain` feature. Both are lazy domains (no reducer, no
/// projection tables) over the generic [`vertex_chain_index_framework`]; the
/// node builder collects [`index::registration`] into the unified indexer.
#[cfg(feature = "chain")]
pub mod index;

/// Number of chunks retained in a reserve sample (the protocol's `SampleSize`).
pub const SAMPLE_SIZE: usize = 16;

pub use anchor::{ClaimAnchor, SampleAnchor};
pub use args::RedistributionArgs;
pub use config::StorageConfig;
pub use filter::{CandidateFilter, FilterRejection, RoundBatches};
pub use neighbourhood::{
    CapacityDoubling, CapacityDoublingError, CommittedDepth, canonical_neighbourhood,
};
pub use proof::{ChunkInclusionProof, ChunkInclusionProofs, ProofError, make_inclusion_proofs};
pub use sample::{SampleItem, reserve_commitment_content, reserve_sample};
pub use witness::{WitnessIndices, witness_indices};
