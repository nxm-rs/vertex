//! Core bandwidth accounting and pricing for data availability.
//!
//! This crate provides the foundational components for data availability incentives:
//!
//! - **Accounting**: Per-peer balance tracking with prepare/apply pattern
//! - **Pricing**: Proximity-based chunk pricing (Bee-compatible)
//!
//! # Units
//!
//! All values are in **Accounting Units (AU)**, not bytes or BZZ tokens.
//! This matches Bee's accounting system where:
//!
//! - Base price per chunk: 10,000 AU
//! - Refresh rate (full node): 4,500,000 AU/second
//! - Payment threshold: 13,500,000 AU
//!
//! # Architecture
//!
//! ```text
//! bandwidth-core (accounting + pricing)
//!        │
//!        ├── pseudosettle (time-based settlement)
//!        │
//!        └── swap (chequebook settlement)
//!               │
//!               └── client (SwarmReader implementation)
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod accounting;
mod pricing;

// Re-export accounting types and constants
pub use accounting::{
    Accounting,
    AccountingAction,
    AccountingConfig,
    AccountingError,
    AccountingPeerHandle,
    CreditAction,
    // Bee-compatible constants (in Accounting Units)
    DEFAULT_BASE_PRICE,
    DEFAULT_EARLY_PAYMENT_PERCENT,
    DEFAULT_LIGHT_FACTOR,
    DEFAULT_LIGHT_REFRESH_RATE,
    DEFAULT_PAYMENT_THRESHOLD,
    DEFAULT_PAYMENT_TOLERANCE_PERCENT,
    DEFAULT_REFRESH_RATE,
    DebitAction,
    // Default configuration implementations
    DefaultAvailabilityConfig,
    NoAvailabilityConfig,
    PeerState,
};

// Re-export pricing types and constants
pub use pricing::{FixedPricer, MAX_PO, Pricer};
