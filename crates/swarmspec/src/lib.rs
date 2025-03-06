//! The spec of a Swarm network
//!
//! This crate defines the specifications and configurations for a Swarm network,
//! including network identification, smart contracts, storage configuration,
//! and incentives settings.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod api;
mod constants;
mod info;
mod lightclient;
mod spec;
mod storage;
mod token;

pub use alloy_chains::{Chain, NamedChain};
pub use vertex_swarm_forks::*;

pub use api::SwarmSpec;
pub use constants::*;
pub use info::SwarmInfo;
pub use lightclient::{LightClient, PseudosettleConfig, SettlementConfig};
pub use spec::{NetworkSpec, SwarmSpecBuilder, SwarmSpecProvider, DEV, MAINNET, TESTNET};
pub use storage::{Storage, StorageContracts};
pub use token::Token;

use core::sync::atomic::{AtomicU64, Ordering};
use vertex_network_primitives_traits::OnceLock;

/// A counter for generating unique network IDs for development/testing
static DEV_NETWORK_ID_COUNTER: AtomicU64 = AtomicU64::new(1337);

/// Generate a unique network ID for development purposes
///
/// This ensures that development/test networks don't clash with each other
pub fn generate_dev_network_id() -> u64 {
    DEV_NETWORK_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Simple utility to create a thread-safe sync cell with a value set.
pub fn once_cell_set<T>(value: T) -> OnceLock<T> {
    let once = OnceLock::new();
    let _ = once.set(value);
    once
}
