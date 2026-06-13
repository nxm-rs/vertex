//! Chequebook factory indexer for Vertex.
//!
//! A per-contract [`Indexer`](vertex_chain_index::Indexer) for the SimpleSwap
//! chequebook factory, driven by the generic
//! [`vertex-chain-index`](vertex_chain_index) engine. It folds the factory's
//! deployment event into a `vertex-storage` set a consumer reads lazily.
//!
//! # What it indexes
//!
//! The SimpleSwapFactory deploys one ERC20SimpleSwap chequebook per node and
//! emits, for each, the single event:
//!
//! - `SimpleSwapDeployed(address contractAddress)` names a newly deployed
//!   chequebook.
//!
//! The event binding and the contract's deployment constants (address and
//! deployment block, for Gnosis Chain mainnet and Sepolia testnet) come from
//! `nectar_contracts`, so the address book is not duplicated here.
//!
//! # The projection
//!
//! State lands in the [`ChequebookFactoryTable`], a set keyed by the deployed
//! chequebook [`Address`](alloy_primitives::Address). Each row records the
//! `(block, log_index)` of the `SimpleSwapDeployed` log that created it.
//! Membership is the fact a consumer needs: ask [`is_factory_deployed`] whether a
//! chequebook came from the factory.
//!
//! # Why this set exists
//!
//! When a cheque arrives, its drawer chequebook must be validated as a genuine,
//! factory-deployed ERC20SimpleSwap before the node trusts it. That validation is
//! a membership query against this set: the indexer records every deployment, and
//! the accounting consumer checks the drawer against the set at the point a cheque
//! is received.
//!
//! # Pure, idempotent fold (no reactions)
//!
//! [`ChequebookFactoryIndexer::apply`](vertex_chain_index::Indexer::apply) is a
//! pure fold into the projection: it writes only the projection table, never
//! calls into accounting, the storer, or any other domain, and re-applying a
//! finalized log produces the same row. A consumer that wants to validate a
//! chequebook does so lazily by reading the set at its own decision point. The
//! chain crate stays domain-agnostic; reactions are the consumer's job. See
//! `CHAIN_REACTIONS_DESIGN.md`.
//!
//! # Placement
//!
//! Native, RPC-and-persistence code intended to run behind a `chain` feature in
//! its consumers and to stay out of the wasm cone. Like the engine, it depends on
//! `alloy` with `default-features = false` and carries no transport of its own.

mod indexer;
mod projection;

pub use indexer::ChequebookFactoryIndexer;
pub use projection::{
    ChequebookFactoryTable, ChequebookFactoryTables, ChequebookKey, DeployedRow, LogPosition,
    apply_deployment, deployment_of, is_factory_deployed,
};

#[cfg(test)]
mod tests;
