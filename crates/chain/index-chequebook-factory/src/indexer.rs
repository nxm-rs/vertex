//! The [`ChequebookFactoryIndexer`]: folds factory deployments into the set.
//!
//! The SimpleSwapFactory (the chequebook factory) deploys an ERC20SimpleSwap
//! chequebook per node and emits `SimpleSwapDeployed(address contractAddress)`
//! for each. This indexer watches that one event, decodes the deployed address,
//! and folds it into the [`ChequebookFactoryTable`] projection: the set of
//! chequebooks the factory has deployed.
//!
//! The fold is pure and idempotent: `apply` writes only to the projection table,
//! never calls into accounting, the storer, or any other domain, and a replayed
//! finalized log re-applies to the same row. Validating a cheque's chequebook as
//! factory-deployed is the consumer's job, done lazily by reading the projection
//! at its own decision point (see `CHAIN_REACTIONS_DESIGN.md`); this crate stays
//! domain-agnostic.

use std::sync::Arc;

use alloy_primitives::Address;
use alloy_rpc_types_eth::{Filter, Log};
use alloy_sol_types::SolEvent;
use nectar_contracts::{ChequebookFactory, IChequebookFactory};
use vertex_chain_index::{IndexError, Indexer};
use vertex_storage::{Database, Tables};

use crate::projection::{ChequebookFactoryTables, LogPosition, apply_deployment};

/// A stable, human-readable name; also the cursor key and metric label.
const INDEXER_NAME: &str = "chequebook_factory";

/// Indexes the chequebook factory into a [`ChequebookFactoryTable`].
///
/// Construct with [`ChequebookFactoryIndexer::new`] from a `nectar_contracts`
/// [`ChequebookFactory`] deployment (its address and deployment block) and a
/// `vertex-storage` [`Database`], then register it with the engine:
///
/// ```ignore
/// use nectar_contracts::mainnet;
/// let indexer = ChequebookFactoryIndexer::new(mainnet::CHEQUEBOOK_FACTORY, db.clone());
/// indexer.init()?;
/// let engine = EventEngine::new(provider, db).register(Arc::new(indexer));
/// ```
///
/// [`ChequebookFactoryTable`]: crate::ChequebookFactoryTable
pub struct ChequebookFactoryIndexer<DB> {
    deployment: ChequebookFactory,
    db: Arc<DB>,
}

impl<DB: Database> ChequebookFactoryIndexer<DB> {
    /// Build an indexer for a chequebook factory deployment over a database.
    ///
    /// `deployment` carries the contract address and its deployment block, so the
    /// same constructor serves mainnet and testnet from the canonical
    /// `nectar_contracts` constants.
    pub fn new(deployment: ChequebookFactory, db: Arc<DB>) -> Self {
        Self { deployment, db }
    }

    /// Create the projection table if it does not exist.
    ///
    /// The engine initializes its own cursor table; the indexer owns its
    /// projection table, so a consumer calls this once before the first run. It
    /// is idempotent.
    pub fn init(&self) -> Result<(), IndexError> {
        ChequebookFactoryTables::init(self.db.as_ref())?;
        Ok(())
    }

    /// Decode one log and return the deployed chequebook address, or `None` if
    /// the log is not a `SimpleSwapDeployed`.
    ///
    /// Split out from [`apply`](Indexer::apply) so the decode is unit-testable in
    /// isolation and `apply` stays a thin store-the-result fold.
    fn decode(log: &Log) -> Result<Option<Address>, IndexError> {
        let Some(topic0) = log.topic0() else {
            return Ok(None);
        };

        if *topic0 == IChequebookFactory::SimpleSwapDeployed::SIGNATURE_HASH {
            let decoded = log
                .log_decode::<IChequebookFactory::SimpleSwapDeployed>()
                .map_err(|e| IndexError::apply(INDEXER_NAME, e.to_string()))?;
            Ok(Some(decoded.inner.contractAddress))
        } else {
            // The filter selects only this topic; tolerate any other log the
            // contract might emit by ignoring it rather than erroring the run.
            Ok(None)
        }
    }
}

impl<DB: Database> Indexer for ChequebookFactoryIndexer<DB> {
    fn name(&self) -> &'static str {
        INDEXER_NAME
    }

    fn start_block(&self) -> u64 {
        self.deployment.block
    }

    fn filter(&self) -> Filter {
        Filter::new()
            .address(self.deployment.address)
            .event_signature(vec![IChequebookFactory::SimpleSwapDeployed::SIGNATURE_HASH])
    }

    fn apply(&self, block: u64, log: &Log) -> Result<(), IndexError> {
        let Some(chequebook) = Self::decode(log)? else {
            return Ok(());
        };

        let log_index = log
            .log_index
            .ok_or(IndexError::apply(INDEXER_NAME, "log missing log_index"))?;
        let pos = LogPosition { block, log_index };

        // Pure projection write: record the deployed chequebook at its source
        // position, guarded so a replayed or reordered log never regresses the
        // set. No side effects, no reactions; a consumer validates a cheque's
        // chequebook lazily by reading the projection.
        self.db
            .update(|tx| apply_deployment(tx, chequebook, pos))
            .map_err(IndexError::from)
    }
}
