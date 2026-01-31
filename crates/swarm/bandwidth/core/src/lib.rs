//! Core bandwidth accounting for Swarm.
//!
//! Per-peer balance tracking with pluggable settlement providers.
//! All values are in **Accounting Units (AU)**, not bytes or BZZ tokens.
//!
//! # Components
//!
//! - [`Accounting`] - Per-peer balance factory with settlement delegation
//! - [`AccountingPeerHandle`] - Handle for recording bandwidth per peer
//! - [`ReceiveAction`] / [`ProvideAction`] - Prepare/apply pattern for balance changes
//! - [`NoSettlement`] - No-op settlement provider
//!
//! Settlement providers (`PseudosettleProvider`, `SwapProvider`) are in sibling crates.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod args;
mod accounting;
mod config;
mod constants;
mod noop;
mod settlement;

pub use accounting::{
    Accounting, AccountingAction, AccountingError, AccountingPeerHandle, PeerState,
    PeerStateSnapshot, ProvideAction, ReceiveAction,
};
pub use args::{BandwidthArgs, BandwidthModeArg};
pub use config::DefaultAccountingConfig;
pub use noop::{NoAccounting, NoPeerBandwidth};
pub use settlement::NoSettlement;
pub use vertex_swarm_bandwidth_pricing::{FixedPricer, NoPricer, Pricer};
