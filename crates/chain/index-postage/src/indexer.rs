//! The [`PostageIndexer`]: the per-contract fold over PostageStamp logs.
//!
//! This is the contract-specific half the generic
//! [`EventEngine`](vertex_chain_index::EventEngine) drives. It declares the
//! contract address, the deployment block, and the `topic0` set of the events it
//! folds, then folds each decoded log into the [`projection`](crate::projection)
//! with a pure, idempotent write.
//!
//! Per the chain-reactions design, `apply` only RECORDS: it writes projection
//! rows and returns. It calls into no domain crate (no storer, no accounting, no
//! postage issuer), fires no reaction, evicts nothing, and gates no node
//! behaviour. Batch expiry has no event to hook (a batch dies when the rising
//! `currentTotalOutPayment` line crosses its static `normalisedBalance`, with no
//! transaction at the crossing), so the validity decision is left to a consumer
//! that recomputes it lazily on the block clock via
//! [`is_batch_valid_now`](crate::projection::is_batch_valid_now).
//!
//! [`EventEngine`]: vertex_chain_index::EventEngine

use std::sync::Arc;

use alloy_primitives::{Address, address};
use alloy_rpc_types_eth::{Filter, Log};
use alloy_sol_types::SolEvent;
use vertex_chain_index::{IndexError, Indexer};
use vertex_storage::{Database, DbTxMut};

use crate::events::{BatchCreated, BatchDepthIncrease, BatchTopUp, Paused, PriceUpdate};
use crate::projection::{
    BatchKey, BatchState, BatchTable, ChainState, ChainStateKey, ChainStateTable, LogPosition,
    PostageTables,
};

/// The PostageStamp contract on Gnosis Chain (id 100).
///
/// This is the specific deployment this indexer projects, distinct from older
/// PostageStamp deployments; the deployment block below pairs with it.
pub const POSTAGE_STAMP_ADDRESS: Address = address!("45a1502382541Cd610CC9068e88727426b696293");

/// The PostageStamp contract deployment block on Gnosis Chain.
///
/// Backfill starts here so the engine never pages the empty range before the
/// contract existed.
pub const POSTAGE_STAMP_DEPLOYMENT_BLOCK: u64 = 31_305_656;

/// The indexer name, used as the engine's cursor key and metric label.
pub const INDEXER_NAME: &str = "postage_stamp";

/// Folds PostageStamp contract logs into the [`crate::projection`].
///
/// Construct with [`new`](PostageIndexer::new) over a `vertex-storage`
/// [`Database`], then register it with an
/// [`EventEngine`](vertex_chain_index::EventEngine). The engine delivers each
/// matching log to [`apply`](Indexer::apply) in canonical `(block, log_index)`
/// order; `apply` decodes it and folds the corresponding projection row.
pub struct PostageIndexer<DB> {
    db: Arc<DB>,
    start_block: u64,
}

impl<DB: Database> PostageIndexer<DB> {
    /// Build the indexer over the database holding the projection tables.
    ///
    /// Backfills from the contract's deployment block. Initializes the projection
    /// tables so the first `apply` has somewhere to write; the engine separately
    /// initializes its own cursor table. Takes the database by `Arc` so the same
    /// handle the [`EventEngine`](vertex_chain_index::EventEngine) drives can be
    /// shared.
    pub fn new(db: Arc<DB>) -> Result<Self, IndexError> {
        Self::with_start_block(db, POSTAGE_STAMP_DEPLOYMENT_BLOCK)
    }

    /// Build the indexer with an explicit backfill start block.
    ///
    /// Production uses [`new`](Self::new) (the deployment block); tests use this
    /// to bound the backfill to a recent window. A start block below the
    /// deployment block has no effect, since the engine never pages before the
    /// contract existed.
    pub fn with_start_block(db: Arc<DB>, start_block: u64) -> Result<Self, IndexError> {
        PostageTables::init(db.as_ref())?;
        Ok(Self { db, start_block })
    }

    /// Decode `log` against `E` or map the decode failure to an apply error.
    fn decode<E: SolEvent>(log: &Log) -> Result<E, IndexError> {
        log.log_decode::<E>()
            .map(|decoded| decoded.inner.data)
            .map_err(|e| IndexError::apply(INDEXER_NAME, e.to_string()))
    }
}

impl<DB: Database> Indexer for PostageIndexer<DB> {
    fn name(&self) -> &'static str {
        INDEXER_NAME
    }

    fn start_block(&self) -> u64 {
        self.start_block
    }

    fn filter(&self) -> Filter {
        Filter::new()
            .address(POSTAGE_STAMP_ADDRESS)
            .event_signature(vec![
                BatchCreated::SIGNATURE_HASH,
                BatchTopUp::SIGNATURE_HASH,
                BatchDepthIncrease::SIGNATURE_HASH,
                PriceUpdate::SIGNATURE_HASH,
                Paused::SIGNATURE_HASH,
            ])
    }

    fn apply(&self, block: u64, log: &Log) -> Result<(), IndexError> {
        let log_index = log
            .log_index
            .ok_or(IndexError::MalformedLog { field: "log_index" })?;
        let pos = LogPosition { block, log_index };

        let topic0 = log
            .topic0()
            .copied()
            .ok_or(IndexError::apply(INDEXER_NAME, "log has no topic0"))?;

        let tx = self.db.as_ref().tx_mut()?;

        match topic0 {
            BatchCreated::SIGNATURE_HASH => {
                let e = Self::decode::<BatchCreated>(log)?;
                upsert_batch(&tx, BatchKey(e.batchId), pos, |batch| match batch {
                    // First sight: open the row from the create payload.
                    None => Some(BatchState {
                        owner: e.owner,
                        depth: e.depth,
                        bucket_depth: e.bucketDepth,
                        normalised_balance: e.normalisedBalance,
                        immutable: e.immutableFlag,
                        start_block: block,
                        source: pos,
                    }),
                    // A create for an already-known batch only refreshes the
                    // mutable fields; `start_block` stays the creation block.
                    Some(mut existing) => {
                        existing.owner = e.owner;
                        existing.depth = e.depth;
                        existing.bucket_depth = e.bucketDepth;
                        existing.normalised_balance = e.normalisedBalance;
                        existing.immutable = e.immutableFlag;
                        existing.source = pos;
                        Some(existing)
                    }
                })?;
            }
            BatchTopUp::SIGNATURE_HASH => {
                let e = Self::decode::<BatchTopUp>(log)?;
                upsert_batch(&tx, BatchKey(e.batchId), pos, |batch| {
                    batch.map(|mut b| {
                        b.normalised_balance = e.normalisedBalance;
                        b.source = pos;
                        b
                    })
                })?;
            }
            BatchDepthIncrease::SIGNATURE_HASH => {
                let e = Self::decode::<BatchDepthIncrease>(log)?;
                upsert_batch(&tx, BatchKey(e.batchId), pos, |batch| {
                    batch.map(|mut b| {
                        b.depth = e.newDepth;
                        b.normalised_balance = e.normalisedBalance;
                        b.source = pos;
                        b
                    })
                })?;
            }
            PriceUpdate::SIGNATURE_HASH => {
                let e = Self::decode::<PriceUpdate>(log)?;
                upsert_chain_state(&tx, pos, |state| {
                    state.fold_price_update(e.price, block);
                    state.source = pos;
                })?;
            }
            Paused::SIGNATURE_HASH => {
                let _ = Self::decode::<Paused>(log)?;
                upsert_chain_state(&tx, pos, |state| {
                    state.paused = true;
                    state.source = pos;
                })?;
            }
            other => {
                return Err(IndexError::apply(
                    INDEXER_NAME,
                    format!("unexpected topic0 {other}"),
                ));
            }
        }

        tx.commit()?;
        Ok(())
    }
}

/// Fold one batch event into [`BatchTable`], guarded by the source position.
///
/// `mutate` receives the loaded row (or `None` on first sight) and returns the
/// row to store, or `None` to leave the table untouched. The supersede guard
/// runs first: a log that is not strictly newer than the stored row is a no-op,
/// so replay and reordering never roll a batch back. A topup or depth-increase
/// for a batch that was never created (only reachable if the create log is
/// outside the indexed window) returns `None` and is dropped rather than
/// fabricating a partial row.
fn upsert_batch<TX, F>(
    tx: &TX,
    key: BatchKey,
    pos: LogPosition,
    mutate: F,
) -> Result<(), IndexError>
where
    TX: DbTxMut,
    F: FnOnce(Option<BatchState>) -> Option<BatchState>,
{
    let existing = tx.get::<BatchTable>(key)?;
    if let Some(current) = existing
        && !current.superseded_by(pos)
    {
        return Ok(());
    }
    if let Some(next) = mutate(existing) {
        tx.put::<BatchTable>(key, next)?;
    }
    Ok(())
}

/// Fold one pricing/pause event into the single-row [`ChainStateTable`], guarded
/// by the source position.
///
/// `mutate` applies the event to the loaded-or-default chain-state. The supersede
/// guard keeps a replayed or reordered log a no-op; a default row is created on
/// first sight so the first `PriceUpdate` or `Paused` log has somewhere to land.
fn upsert_chain_state<TX, F>(tx: &TX, pos: LogPosition, mutate: F) -> Result<(), IndexError>
where
    TX: DbTxMut,
    F: FnOnce(&mut ChainState),
{
    let existing = tx.get::<ChainStateTable>(ChainStateKey)?;
    if let Some(current) = existing
        && !current.superseded_by(pos)
    {
        return Ok(());
    }
    let mut state = existing.unwrap_or_default();
    mutate(&mut state);
    tx.put::<ChainStateTable>(ChainStateKey, state)?;
    Ok(())
}
