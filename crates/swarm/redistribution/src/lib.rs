//! Swarm redistribution (storage incentives).
//!
//! Two concerns live in this crate:
//!
//! - **Configuration** for participation in the redistribution game
//!   ([`RedistributionArgs`], [`StorageConfig`]).
//! - **Consensus primitives**: a deterministic reserve sample over a
//!   neighbourhood and a proof of entitlement over that sample. These are pure
//!   functions of their inputs (no I/O, no storage, no node, no async), so they
//!   produce identical results on every participating node.
//!
//! # Consensus conformance
//!
//! The transformed addresses, the selected sample and the inclusion proofs must
//! be byte-exact, because the same values are verified on chain by the
//! `Redistribution.sol` storage-incentives contract; any divergence loses (or is
//! slashed in) the round. The algorithm is the one fixed by the Swarm
//! storage-incentives protocol and the contract's verification logic, validated
//! against the canonical Swarm reference vectors in `tests/`.
//!
//! The anchor-keyed transformed address (the value the sample is ordered by) is
//! a nectar primitive
//! ([`AnyChunk::transformed_address`](nectar_primitives::AnyChunk::transformed_address));
//! this crate consumes it rather than re-deriving it.
//!
//! # Building blocks
//!
//! - [`SampleAnchor`] / [`ClaimAnchor`]: the two per-round reserve salts as
//!   distinct types, so they cannot be transposed.
//! - [`CommittedDepth`] / [`canonical_neighbourhood`]: the chunk addresses a
//!   node is responsible for at a given depth.
//! - [`SampleItem`] / [`reserve_sample`]: the [`SAMPLE_SIZE`] chunks with the
//!   smallest transformed addresses.
//! - [`WitnessIndices`] / [`witness_indices`]: the sample slots a claim opens.
//! - [`make_inclusion_proofs`] / [`ChunkInclusionProof`]: the proof of
//!   entitlement submitted to the contract.

mod anchor;
mod args;
mod config;
mod neighbourhood;
mod proof;
mod sample;
mod witness;

/// Number of chunks retained in a reserve sample (the protocol's `SampleSize`).
pub const SAMPLE_SIZE: usize = 16;

pub use anchor::{ClaimAnchor, SampleAnchor};
pub use args::RedistributionArgs;
pub use config::StorageConfig;
pub use neighbourhood::{CommittedDepth, canonical_neighbourhood};
pub use proof::{ChunkInclusionProof, ChunkInclusionProofs, ProofError, make_inclusion_proofs};
pub use sample::{SampleItem, reserve_commitment_content, reserve_sample};
pub use witness::{WitnessIndices, witness_indices};
