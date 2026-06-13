//! Swap (settlement) price oracle indexer for Vertex.
//!
//! A per-contract [`Indexer`](vertex_chain_index::Indexer) for the swap price
//! oracle, driven by the generic [`vertex-chain-index`](vertex_chain_index)
//! engine. It folds the oracle's two events into a small `vertex-storage`
//! projection a consumer reads lazily.
//!
//! # What it indexes
//!
//! The swap price oracle publishes the two scalars the node needs to price
//! settlement cheques:
//!
//! - `PriceUpdate(uint256 price)` sets the swap exchange rate.
//! - `ChequeValueDeductionUpdate(uint256 chequeValueDeduction)` sets the cheque
//!   value deduction.
//!
//! The event bindings and the contract's deployment constants (address and
//! deployment block, for Gnosis Chain mainnet and Sepolia testnet) come from
//! `nectar_contracts`, so the address book is not duplicated here.
//!
//! # The projection
//!
//! State lands in the [`SwapPriceTable`], a two-row table keyed by
//! [`SwapPriceField`]: one row for the exchange rate, one for the deduction. Each
//! row records the value and the `(block, log_index)` that set it. Read the
//! current values with [`read_field`].
//!
//! # Pure, idempotent fold (no reactions)
//!
//! [`SwapPriceIndexer::apply`](vertex_chain_index::Indexer::apply) is a pure fold
//! into the projection: it writes only the projection table, never calls into
//! accounting, the storer, or any other domain, and re-applying a finalized log
//! produces the same row. A consumer that wants to react to a rate change (say,
//! re-pricing a cheque) does so lazily by reading the projection at its own
//! decision point. The chain crate stays domain-agnostic; reactions are the
//! consumer's job. See `CHAIN_REACTIONS_DESIGN.md`.
//!
//! # Placement
//!
//! Native, RPC-and-persistence code intended to run behind a `chain` feature in
//! its consumers and to stay out of the wasm cone. Like the engine, it depends
//! on `alloy` with `default-features = false` and carries no transport of its
//! own.

mod indexer;
mod projection;

pub use indexer::SwapPriceIndexer;
pub use projection::{
    LogPosition, SwapPriceField, SwapPriceRow, SwapPriceTable, SwapPriceTables, apply_update,
    read_field,
};

#[cfg(test)]
mod tests;
