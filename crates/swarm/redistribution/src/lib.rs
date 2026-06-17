//! Swarm redistribution (storage incentives) configuration.
//!
//! This crate provides configuration for the Swarm redistribution game,
//! which incentivizes nodes to store and serve chunks within their
//! neighborhood of responsibility.

mod args;
mod config;
mod redistribution;

pub use args::RedistributionArgs;
pub use config::StorageConfig;
pub use redistribution::{
    ChunkInclusionProof, ChunkInclusionProofs, ProofError, SAMPLE_SIZE, SampleItem, WitnessIndices,
    canonical_neighbourhood, make_inclusion_proofs, reserve_commitment_content, reserve_sample,
    witness_indices,
};
