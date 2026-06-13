//! Engine metrics: lazy counters for pages, logs, and errors.
//!
//! Counters are labelled by indexer `name` so an operator can attribute paging
//! volume and fold throughput to a specific contract. Errors are recorded via
//! [`IndexError::record`](crate::IndexError) with a `reason` label, matching the
//! repo's error-counter convention.

use std::sync::LazyLock;

use vertex_metrics::metrics_crate::{Counter, counter};

/// Number of `eth_getLogs` pages fetched, per indexer.
pub(crate) fn pages_total(name: &'static str) -> Counter {
    counter!("chain_index_pages_total", "indexer" => name)
}

/// Number of logs applied, per indexer.
pub(crate) fn logs_total(name: &'static str) -> Counter {
    counter!("chain_index_logs_total", "indexer" => name)
}

/// Number of cursor checkpoints committed, per indexer.
pub(crate) fn checkpoints_total(name: &'static str) -> Counter {
    counter!("chain_index_checkpoints_total", "indexer" => name)
}

/// Number of adaptive page-size shrink events (provider range/limit errors),
/// across all indexers.
pub(crate) static PAGE_SHRINKS_TOTAL: LazyLock<Counter> =
    LazyLock::new(|| counter!("chain_index_page_shrinks_total"));
