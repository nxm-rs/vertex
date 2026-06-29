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
//! - [`NoSettlement`] - No-op settlement provider
//!
//! Settlement providers (`PseudosettleProvider`, `SwapProvider`) are in sibling crates.
//!
//! [`Accounting`] also implements the `Ledger` and `AdmissionControl` surfaces
//! (from `vertex-swarm-api`), so peer selection and pacing can consume accounting
//! state without depending on this crate's internals.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod accounting;
pub mod args;
mod builder;
mod client_accounting;
mod config;
mod constants;
mod noop;
mod settlement;

pub use accounting::{
    Accounting, AccountingError, AccountingPeerHandle, PeerState, Provide, Receive, Reservation,
};
pub use args::BandwidthArgs;
pub use builder::{AccountingBuilder, NoAccountingBuilder};
pub use client_accounting::ClientAccounting;
pub use config::{BandwidthConfig, DefaultBandwidthConfig};
pub use noop::{NoAccounting, NoPeerBandwidth, NoProvideAction, NoReceiveAction};
pub use settlement::NoSettlement;
pub use vertex_swarm_accounting_pricing::{FixedPricer, FixedPricingConfig, NoPricer};
