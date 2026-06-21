//! The swap-price-oracle domain's chain indexing (behind `chain`).
//!
//! A lazy domain: stores the two `*Update` events verbatim and computes the
//! exchange rate and cheque value deduction by a last-write-wins walk on read
//! (see [`views`]); no reducer, no projection tables.

mod register;
pub mod views;

pub use register::{TAG_SWAP_ORACLE, registration};

#[cfg(test)]
mod tests;
