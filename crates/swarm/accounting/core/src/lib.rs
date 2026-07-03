//! Core bandwidth accounting for Swarm.
//!
//! Per-peer balance tracking with pluggable settlement providers.
//! All values are in **Accounting Units (AU)**, not bytes or BZZ tokens.
//!
//! # Components
//!
//! - [`Accounting`] - Per-peer balance factory with settlement delegation
//! - [`AccountingBuilder`] - Builder for constructing accounting with pricing
//! - [`AccountingPeerHandle`] - Handle for recording bandwidth per peer
//! - [`Reservation`] - Typed receive/provide reservation legs
//!
//! Settlement providers (`PseudosettleProvider`, `SwapProvider`) are in sibling crates.
//!
//! [`Accounting`] also implements the `Ledger` and `AdmissionControl` surfaces
//! (from `vertex-swarm-api`), so peer selection and pacing can consume accounting
//! state without depending on this crate's internals.
//!
//! # Construction
//!
//! Construction is two-phase: [`AccountingBuilder::build`] produces a
//! [`ClientAccounting`] with the ledger, pricer, and settlement providers
//! embedded; node assembly (`assemble_client_core` in the node crate) then
//! wires the selector, origin gate, settlement trigger, and settlement
//! services around it. The invariant is a single shared instance: every
//! consumer must read the same per-peer balances, so assembly shares one
//! `Arc` of the one build rather than building twice.
//!
//! # Commit points
//!
//! One [`Reservation`] contract, two commit points, coupled to the dispatch
//! discipline. An origin leg books at dispatch (the `OriginAccounting` gate:
//! reserve and apply in one step, refund only a confirmed no-charge) because
//! origin dispatch may race and a losing raced leg is cancelled by drop: a
//! dropped reservation releases, so a commit deferred to delivery could
//! un-book debt for bytes the wire may still deliver. A relay leg defers its
//! commit to the verified answer (reserve, apply on verify, release on drop),
//! which is safe because relay walks are sequential and never cancel
//! mid-flight, and strictly fairer to the peer. Never unify the two: the
//! dropped-race-loser-keeps-commit guarantee is the reason the origin books
//! at dispatch.

mod accounting;
#[cfg(feature = "cli")]
pub mod args;
mod builder;
mod client_accounting;
mod config;
mod constants;
#[cfg(test)]
mod settlement;

pub use accounting::{
    Accounting, AccountingError, AccountingPeerHandle, PeerState, Provide, Receive, Reservation,
    ReservationCaps,
};
#[cfg(feature = "cli")]
pub use args::AccountingArgs;
pub use builder::AccountingBuilder;
pub use client_accounting::ClientAccounting;
pub use config::{AccountingConfig, DefaultAccountingConfig};
#[cfg(test)]
pub(crate) use settlement::NoSettlement;
pub use vertex_swarm_accounting_pricing::{FixedPricer, FixedPricingConfig, NoPricer};
