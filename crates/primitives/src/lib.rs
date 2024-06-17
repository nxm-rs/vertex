//! Commonly used types in beers.
//!
//! This crate contains primitive types and helper functions.

#![doc(
    issue_tracker_base_url = "https://github.com/rndlabs/beers/issues/"
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
// TODO: remove when https://github.com/proptest-rs/proptest/pull/427 is merged
#![allow(unknown_lints, non_local_definitions)]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

// const STAMP_INDEX_SIZE: u8 = 8;
// const STAMP_TIMESTAMP_SIZE: u8 = 8;
// const SPAN_SIZE: u16 = 8;
// const SECTION_SIZE: u16 = 32;
// const BRANCHES: u16 = 128;
// const ENCRYPTED_BRANCHES: u16 = BRANCHES / 2;
// const BMT_BRANCHES: u8 = 128;
// const CHUNK_SIZE: u16 = SECTION_SIZE * BRANCHES;
const HASH_SIZE: usize = 32;
const MAX_PO: u8 = 31;
const EXTENDED_PO: u8 = MAX_PO + 5;
// const MAX_BINS: u8 = MAX_PO + 1;
// const CHUNK_WITH_SPAN_SIZE: u16 = CHUNK_SIZE + SPAN_SIZE;
// const SOC_SIGNATURE_SIZE: u16 = 65;
// const SOC_MIN_CHUNK_SIZE: u16 = HASH_SIZE + SOC_SIGNATURE_SIZE + SPAN_SIZE;
// const SOC_MAX_CHUNK_SIZE: u16 = SOC_MIN_CHUNK_SIZE + CHUNK_SIZE;

pub mod distaddr;
pub mod overlay;
pub mod proximity;
mod swarm;
pub use swarm::{
    NamedSwarm, Swarm,
};

mod bmt;
pub use bmt::*;

mod postage;
pub use postage::*;

// mod manifest;
// pub use manifest::*;

mod nodeaddr;
pub use nodeaddr::*;