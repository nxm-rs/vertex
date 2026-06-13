//! The [`Indexer`] trait: the per-contract contract the engine drives.
//!
//! An indexer is the minimal, contract-specific half of the indexing story. It
//! declares what to watch (a deployment start block and a log [`Filter`]) and
//! how to fold a single decoded log into its own state. Everything else (paging,
//! ordering, the finalized-tag reorg boundary, cursor persistence, restart
//! idempotency, metrics) is the engine's job, written once in [`EventEngine`].
//!
//! [`EventEngine`]: crate::EventEngine

use alloy_rpc_types_eth::{Filter, Log};

use crate::IndexError;

/// A contract-specific event consumer the [`EventEngine`] drives.
///
/// Implementors fold logs into their own indexed state (typically a
/// `vertex-storage` table). The engine guarantees logs reach [`apply`] in
/// `(block_number, log_index)` order, within the canonical, already-finalized
/// range, and that re-applying an already-seen range on restart is the
/// implementor's only idempotency concern (the engine never advances the cursor
/// past an `apply` that returned an error).
///
/// [`apply`]: Indexer::apply
/// [`EventEngine`]: crate::EventEngine
pub trait Indexer: Send + Sync {
    /// A stable, human-readable name, used as the cursor key and in metrics and
    /// logs. Must be unique per engine instance.
    fn name(&self) -> &'static str;

    /// The contract deployment block. Backfill starts here (or at the persisted
    /// cursor, whichever is later), so the engine never pages the empty range
    /// before the contract existed.
    fn start_block(&self) -> u64;

    /// The log filter selecting this indexer's events: the contract
    /// address(es) and the event `topic0` set. The engine overrides the
    /// filter's block range per page and leaves the address and topic
    /// constraints intact.
    fn filter(&self) -> Filter;

    /// Fold one log into indexed state.
    ///
    /// Called once per matching log, in `(block_number, log_index)` order, with
    /// `block` equal to the log's block number. Decode the log here with
    /// `log.log_decode::<MyEvent>()`. Returning an error stops this indexer's
    /// run loop without advancing its cursor, so the failed range is retried on
    /// the next run rather than skipped.
    ///
    /// State mutations made here are committed atomically with the cursor by the
    /// engine; see [`EventEngine`](crate::EventEngine) for how an indexer shares
    /// the engine's write transaction.
    fn apply(&self, block: u64, log: &Log) -> Result<(), IndexError>;

    /// Roll indexed state back to before `from_block`.
    ///
    /// Reserved for optimistic head-tracking: the MVP engine indexes only up to
    /// the chain's finalized tag, which never reorgs, so this is never called by
    /// the current engine and defaults to a no-op. An implementor that opts into
    /// head-tracking (a documented future enhancement) implements this to undo
    /// the folds for the reorged-out range `from_block..`.
    fn revert(&self, from_block: u64) -> Result<(), IndexError> {
        let _ = from_block;
        Ok(())
    }
}
