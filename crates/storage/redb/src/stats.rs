//! Periodic database stats collection for gauge metrics.

use metrics::gauge;
use redb::{ReadableTableMetadata, TableHandle};

use crate::RedbDatabase;

/// Collect and emit gauge metrics for a redb database.
///
/// Opens a read transaction to gather per-table stats, queries file size
/// from the filesystem, and reads cache stats. Tables are discovered
/// automatically via `list_tables()`.
///
/// Call this periodically (e.g. every 30s) from the application layer.
pub fn collect_db_metrics(db: &RedbDatabase) {
    // File size from filesystem.
    if let Some(path) = db.path()
        && let Ok(meta) = std::fs::metadata(path)
    {
        gauge!("redb_file_size_bytes").set(meta.len() as f64);
    }

    // Cache stats.
    let cache = db.inner().cache_stats();
    gauge!("redb_cache_evictions_total").set(cache.evictions() as f64);

    // Per-table stats via read transaction.
    let Ok(tx) = db.inner().begin_read() else {
        return;
    };

    // Discover all tables dynamically.
    let Ok(tables) = tx.list_tables() else {
        return;
    };
    let handles: Vec<_> = tables.collect();

    let mut total_stored: u64 = 0;
    let mut total_metadata: u64 = 0;
    let mut total_fragmented: u64 = 0;

    for handle in &handles {
        let name = handle.name().to_string();
        let Ok(table) = tx.open_untyped_table(handle.clone()) else {
            continue;
        };

        // Row count (db-agnostic metric).
        if let Ok(len) = table.len() {
            gauge!("db_entries", "table" => name.clone()).set(len as f64);
        }

        // redb-specific per-table stats.
        if let Ok(stats) = table.stats() {
            gauge!("redb_stored_bytes", "table" => name.clone()).set(stats.stored_bytes() as f64);
            gauge!("redb_metadata_bytes", "table" => name.clone())
                .set(stats.metadata_bytes() as f64);
            gauge!("redb_fragmented_bytes", "table" => name.clone())
                .set(stats.fragmented_bytes() as f64);
            gauge!("redb_tree_height", "table" => name.clone()).set(stats.tree_height() as f64);
            gauge!("redb_leaf_pages", "table" => name.clone()).set(stats.leaf_pages() as f64);
            gauge!("redb_branch_pages", "table" => name.clone()).set(stats.branch_pages() as f64);

            total_stored += stats.stored_bytes();
            total_metadata += stats.metadata_bytes();
            total_fragmented += stats.fragmented_bytes();
        }
    }

    // Aggregated totals across all discovered tables.
    gauge!("redb_stored_bytes_total").set(total_stored as f64);
    gauge!("redb_metadata_bytes_total").set(total_metadata as f64);
    gauge!("redb_fragmented_bytes_total").set(total_fragmented as f64);
}
