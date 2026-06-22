//! Accounting errors (threshold violations, settlement failures).
//!
//! The type lives in `vertex-swarm-api` so `SwarmError` can carry it typed; it
//! is re-exported here for the original `crate::accounting::error` path.

pub use vertex_swarm_api::AccountingError;
