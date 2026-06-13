//! The [`SwapPriceIndexer`]: folds swap price oracle events into the projection.
//!
//! The swap (settlement) price oracle publishes two scalars the node reads when
//! pricing settlement cheques: the BZZ/xDAI exchange rate (`PriceUpdate`) and the
//! cheque value deduction (`ChequeValueDeductionUpdate`). This indexer watches
//! both, decodes each log, and folds it into the [`SwapPriceTable`] projection.
//!
//! The fold is pure and idempotent: `apply` writes only to the projection table,
//! never calls into accounting or any other domain, and a replayed finalized log
//! re-applies to the same row with the same value. Reacting to a rate change (for
//! example re-pricing a cheque) is the consumer's job, done lazily by reading the
//! projection at its own decision point; this crate stays domain-agnostic.

use std::sync::Arc;

use alloy_rpc_types_eth::{Filter, Log};
use alloy_sol_types::SolEvent;
use nectar_contracts::{ISwapPriceOracle, SwapPriceOracle};
use vertex_chain_index::{IndexError, Indexer};
use vertex_storage::{Database, Tables};

use crate::projection::{LogPosition, SwapPriceField, SwapPriceTables, apply_update};

/// A stable, human-readable name; also the cursor key and metric label.
const INDEXER_NAME: &str = "swap_price_oracle";

/// Indexes the swap (settlement) price oracle into a [`SwapPriceTable`].
///
/// Construct with [`SwapPriceIndexer::new`] from a `nectar_contracts`
/// [`SwapPriceOracle`] deployment (its address and deployment block) and a
/// `vertex-storage` [`Database`], then register it with the engine:
///
/// ```ignore
/// use nectar_contracts::mainnet;
/// let indexer = SwapPriceIndexer::new(mainnet::SWAP_PRICE_ORACLE, db.clone());
/// indexer.init()?;
/// let engine = EventEngine::new(provider, db).register(Arc::new(indexer));
/// ```
///
/// [`SwapPriceTable`]: crate::SwapPriceTable
pub struct SwapPriceIndexer<DB> {
    deployment: SwapPriceOracle,
    db: Arc<DB>,
}

impl<DB: Database> SwapPriceIndexer<DB> {
    /// Build an indexer for a swap price oracle deployment over a database.
    ///
    /// `deployment` carries the contract address and its deployment block, so the
    /// same constructor serves mainnet and testnet from the canonical
    /// `nectar_contracts` constants.
    pub fn new(deployment: SwapPriceOracle, db: Arc<DB>) -> Self {
        Self { deployment, db }
    }

    /// Create the projection table if it does not exist.
    ///
    /// The engine initializes its own cursor table; the indexer owns its
    /// projection table, so a consumer calls this once before the first run. It
    /// is idempotent.
    pub fn init(&self) -> Result<(), IndexError> {
        SwapPriceTables::init(self.db.as_ref())?;
        Ok(())
    }

    /// Decode one log and return the field and value it updates, or `None` if the
    /// log is not one of the two indexed events.
    ///
    /// Split out from [`apply`](Indexer::apply) so the decode is unit-testable in
    /// isolation and `apply` stays a thin store-the-result fold.
    fn decode(log: &Log) -> Result<Option<(SwapPriceField, alloy_primitives::U256)>, IndexError> {
        let Some(topic0) = log.topic0() else {
            return Ok(None);
        };

        if *topic0 == ISwapPriceOracle::PriceUpdate::SIGNATURE_HASH {
            let decoded = log
                .log_decode::<ISwapPriceOracle::PriceUpdate>()
                .map_err(|e| IndexError::apply(INDEXER_NAME, e.to_string()))?;
            Ok(Some((SwapPriceField::ExchangeRate, decoded.inner.price)))
        } else if *topic0 == ISwapPriceOracle::ChequeValueDeductionUpdate::SIGNATURE_HASH {
            let decoded = log
                .log_decode::<ISwapPriceOracle::ChequeValueDeductionUpdate>()
                .map_err(|e| IndexError::apply(INDEXER_NAME, e.to_string()))?;
            Ok(Some((
                SwapPriceField::ChequeValueDeduction,
                decoded.inner.chequeValueDeduction,
            )))
        } else {
            // The filter selects only these two topics, but the contract also
            // emits `OwnershipTransferred`; tolerate an unrelated log by ignoring
            // it rather than erroring the run loop.
            Ok(None)
        }
    }
}

impl<DB: Database> Indexer for SwapPriceIndexer<DB> {
    fn name(&self) -> &'static str {
        INDEXER_NAME
    }

    fn start_block(&self) -> u64 {
        self.deployment.block
    }

    fn filter(&self) -> Filter {
        Filter::new()
            .address(self.deployment.address)
            .event_signature(vec![
                ISwapPriceOracle::PriceUpdate::SIGNATURE_HASH,
                ISwapPriceOracle::ChequeValueDeductionUpdate::SIGNATURE_HASH,
            ])
    }

    fn apply(&self, block: u64, log: &Log) -> Result<(), IndexError> {
        let Some((field, value)) = Self::decode(log)? else {
            return Ok(());
        };

        let log_index = log
            .log_index
            .ok_or(IndexError::apply(INDEXER_NAME, "log missing log_index"))?;
        let pos = LogPosition { block, log_index };

        // Pure projection write: store the value at its source position, guarded
        // so a replayed or reordered log never rolls the row back. No side
        // effects, no reactions; a consumer reads the projection lazily.
        self.db
            .update(|tx| apply_update(tx, field, value, pos))
            .map_err(IndexError::from)
    }
}
