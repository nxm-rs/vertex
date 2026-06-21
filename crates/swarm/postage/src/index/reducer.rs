//! Folds the PostageStamp batch-lifecycle events into the eager
//! [`Batches`](crate::store) projection (the table
//! [`DbBatchStore`](crate::DbBatchStore) persists). `PriceUpdate` is left
//! verbatim for the lazy
//! [`total_out_payment_at`](crate::index::total_out_payment_at) fold.

use alloy_sol_types::SolEvent;
use nectar_postage::Batch;
use vertex_chain_index_framework::{ContractTag, EventKey, Reducer, StoredEvent};
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut};

use crate::index::TAG_POSTAGE;
use crate::index::abi::events;
use crate::store::{BatchIdKey, Batches};

/// The postage reducer, dispatched by the framework for [`TAG_POSTAGE`].
#[derive(Debug, Clone, Copy, Default)]
pub struct PostageReducer;

impl<DB: Database> Reducer<DB> for PostageReducer {
    fn tag(&self) -> ContractTag {
        TAG_POSTAGE
    }

    fn reduce(&self, tx: &DB::TXMut, key: EventKey, ev: &StoredEvent) -> Result<(), DatabaseError> {
        apply_event::<DB>(tx, key.block, ev)
    }

    fn rebuild(
        &self,
        tx: &DB::TXMut,
        surviving: &[(EventKey, StoredEvent)],
    ) -> Result<(), DatabaseError> {
        // Re-fold the survivors from scratch: a batch created before the boundary
        // but mutated within the reverted range rolls back to its surviving state,
        // which a create-block filter could not catch.
        tx.clear::<Batches>()?;
        for (key, ev) in surviving {
            apply_event::<DB>(tx, key.block, ev)?;
        }
        Ok(())
    }
}

/// Fold one stored PostageStamp event into [`Batches`]. A non-batch event or a
/// decode miss is a skip, never an error, so a malformed body cannot wedge the
/// shared cursor. Shared by the live and rebuild paths.
fn apply_event<DB: Database>(
    tx: &DB::TXMut,
    block: u64,
    ev: &StoredEvent,
) -> Result<(), DatabaseError> {
    let data = ev.log_data();

    if ev.topic0 == events::BatchCreated::SIGNATURE_HASH {
        let Ok(e) = events::BatchCreated::decode_log_data(&data) else {
            return Ok(());
        };
        let Ok(value) = u128::try_from(e.normalisedBalance) else {
            return Ok(());
        };
        // Preserve the original creation block if the batch is already known: a
        // replayed create only refreshes the mutable fields.
        let start = tx
            .get::<Batches>(BatchIdKey(e.batchId))?
            .map_or(block, |b| b.start());
        let batch = Batch::new(
            e.batchId,
            value,
            start,
            e.owner,
            e.depth,
            e.bucketDepth,
            e.immutableFlag,
        );
        tx.put::<Batches>(BatchIdKey(e.batchId), batch)?;
    } else if ev.topic0 == events::BatchTopUp::SIGNATURE_HASH {
        let Ok(e) = events::BatchTopUp::decode_log_data(&data) else {
            return Ok(());
        };
        let Ok(value) = u128::try_from(e.normalisedBalance) else {
            return Ok(());
        };
        if let Some(mut batch) = tx.get::<Batches>(BatchIdKey(e.batchId))? {
            batch.set_value(value);
            tx.put::<Batches>(BatchIdKey(e.batchId), batch)?;
        }
    } else if ev.topic0 == events::BatchDepthIncrease::SIGNATURE_HASH {
        let Ok(e) = events::BatchDepthIncrease::decode_log_data(&data) else {
            return Ok(());
        };
        // A dilution re-normalises the balance as it raises the depth, so apply
        // both from the one event.
        let Ok(value) = u128::try_from(e.normalisedBalance) else {
            return Ok(());
        };
        if let Some(mut batch) = tx.get::<Batches>(BatchIdKey(e.batchId))? {
            batch.set_depth(e.newDepth);
            batch.set_value(value);
            tx.put::<Batches>(BatchIdKey(e.batchId), batch)?;
        }
    }

    Ok(())
}
