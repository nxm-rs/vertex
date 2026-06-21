//! On-chain chequebook-factory indexing (behind the `chain` feature).
//!
//! A lazy domain: records `SimpleSwapDeployed` verbatim and answers
//! deployed-set queries by folding on read; no reducer, no projection tables.

mod canonical;
mod register;
pub mod views;

pub use register::{TAG_CHEQUEBOOK, registration};

#[cfg(test)]
mod tests;
