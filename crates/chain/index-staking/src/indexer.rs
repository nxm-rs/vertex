//! The [`StakingIndexer`]: a pure fold of `StakeRegistry` events into the
//! staking projection.
//!
//! This is the contract-specific half the [`EventEngine`] drives. It declares
//! the deployment block and the log filter (the registry address and the five
//! staking `topic0`s), and folds each decoded log into the [`StakeProjection`]
//! tables. Per `CHAIN_REACTIONS_DESIGN.md`, [`apply`] is a pure, idempotent fold:
//! it has no side effects, calls into no domain layer, and a replayed finalized
//! log re-applies to the same row.
//!
//! [`EventEngine`]: vertex_chain_index::EventEngine
//! [`apply`]: vertex_chain_index::Indexer::apply

use std::sync::Arc;

use alloy_primitives::{Address, U256, address};
use alloy_rpc_types_eth::{Filter, Log};
use alloy_sol_types::{SolEvent, sol};
use nectar_contracts::IStakeRegistry;
use vertex_chain_index::{IndexError, Indexer};
use vertex_storage::{Database, DbTxMut, Tables};

use crate::projection::{LogPos, OwnerStake, StakingTables, fold_owner};

sol! {
    /// The one `StakeRegistry` event the shared `IStakeRegistry` interface does
    /// not carry. Declared here from the deployment ABI so the indexer folds it
    /// alongside the shared events; both `owner` and `overlay` are non-indexed.
    #[allow(missing_docs)]
    event OverlayChanged(address owner, bytes32 overlay);
}

/// The Gnosis Chain mainnet `StakeRegistry` deployment this indexer follows.
///
/// The address and block come from the storage-incentives mainnet deployment
/// manifest. They pin the exact contract instance whose events this projection
/// reflects; backfill starts at the deployment block so the engine never pages
/// the empty pre-deployment range.
pub const STAKE_REGISTRY: Address = address!("da2a16EE889E7F04980A8d597b48c8D51B9518F4");

/// The block the [`STAKE_REGISTRY`] contract was deployed at.
pub const DEPLOYMENT_BLOCK: u64 = 40_430_237;

/// The indexer name, used as the engine's cursor key and metric label.
const INDEXER_NAME: &str = "staking_registry";

/// Folds `StakeRegistry` events into the staking projection.
///
/// One instance, registered with an [`EventEngine`], maintains the per-owner
/// stake rows and the staked-overlay set in `vertex-storage`. It owns a
/// `vertex-storage` [`Database`] handle (the same backend the cursor lives in)
/// and writes its projection through its own transaction; the engine bridges the
/// projection commit and the cursor commit with idempotent replay, which this
/// fold's monotonic `(block, log_index)` supersede rule satisfies.
///
/// [`EventEngine`]: vertex_chain_index::EventEngine
pub struct StakingIndexer<DB> {
    db: Arc<DB>,
    start_block: u64,
}

impl<DB: Database> StakingIndexer<DB> {
    /// Build an indexer over `db`, starting backfill at the deployment block.
    pub fn new(db: Arc<DB>) -> Result<Self, IndexError> {
        StakingTables::init(db.as_ref())?;
        Ok(Self {
            db,
            start_block: DEPLOYMENT_BLOCK,
        })
    }

    /// Override the backfill start block. Mainly for tests that fold synthetic
    /// logs without the full pre-deployment range.
    pub fn with_start_block(mut self, block: u64) -> Self {
        self.start_block = block;
        self
    }

    /// The `(block, log_index)` position of `log`, the supersede key.
    fn position(log: &Log) -> Result<LogPos, IndexError> {
        Ok(LogPos {
            block: log.block_number.ok_or(IndexError::MalformedLog {
                field: "block_number",
            })?,
            index: log
                .log_index
                .ok_or(IndexError::MalformedLog { field: "log_index" })?,
        })
    }

    /// Fold one decoded event into `owner`'s row, superseding only on a strictly
    /// later log position.
    ///
    /// `mutate` produces the next field values from the previous row; the
    /// position guard wraps it so a replayed or out-of-order log is a no-op. This
    /// is the single idempotency choke point: every event routes through it.
    fn supersede<F>(&self, owner: Address, pos: LogPos, mutate: F) -> Result<(), IndexError>
    where
        F: FnOnce(OwnerStake) -> OwnerStake,
    {
        let tx = self.db.tx_mut()?;
        fold_owner(&tx, owner, |prev| {
            // A log at or before the row's recorded position has already been
            // folded (or arrived out of order); skip it so replay is a no-op and
            // state only ever moves forward. A never-seen row carries the minimum
            // position sentinel, so the owner's first real log always supersedes.
            if pos <= prev.seen_at {
                return None;
            }
            let mut next = mutate(prev);
            next.seen_at = pos;
            Some(next)
        })?;
        tx.commit()?;
        Ok(())
    }
}

impl<DB: Database> Indexer for StakingIndexer<DB> {
    fn name(&self) -> &'static str {
        INDEXER_NAME
    }

    fn start_block(&self) -> u64 {
        self.start_block
    }

    fn filter(&self) -> Filter {
        Filter::new().address(STAKE_REGISTRY).event_signature(vec![
            IStakeRegistry::StakeUpdated::SIGNATURE_HASH,
            IStakeRegistry::StakeFrozen::SIGNATURE_HASH,
            IStakeRegistry::StakeSlashed::SIGNATURE_HASH,
            IStakeRegistry::StakeWithdrawn::SIGNATURE_HASH,
            OverlayChanged::SIGNATURE_HASH,
        ])
    }

    fn apply(&self, _block: u64, log: &Log) -> Result<(), IndexError> {
        let pos = Self::position(log)?;
        let topic0 = log.topic0().copied();

        match topic0 {
            Some(sig) if sig == IStakeRegistry::StakeUpdated::SIGNATURE_HASH => {
                let ev = log
                    .log_decode::<IStakeRegistry::StakeUpdated>()
                    .map_err(|e| IndexError::apply(INDEXER_NAME, e.to_string()))?
                    .inner
                    .data;
                self.supersede(ev.owner, pos, |mut s| {
                    s.committed = ev.committedStake;
                    s.potential = ev.potentialStake;
                    s.overlay = ev.overlay;
                    s.height = ev.height;
                    s.last_updated_block = ev.lastUpdatedBlock;
                    s
                })
            }
            Some(sig) if sig == IStakeRegistry::StakeFrozen::SIGNATURE_HASH => {
                let ev = log
                    .log_decode::<IStakeRegistry::StakeFrozen>()
                    .map_err(|e| IndexError::apply(INDEXER_NAME, e.to_string()))?
                    .inner
                    .data;
                self.supersede(ev.frozen, pos, |mut s| {
                    s.frozen_until = ev.time;
                    s
                })
            }
            Some(sig) if sig == IStakeRegistry::StakeSlashed::SIGNATURE_HASH => {
                let ev = log
                    .log_decode::<IStakeRegistry::StakeSlashed>()
                    .map_err(|e| IndexError::apply(INDEXER_NAME, e.to_string()))?
                    .inner
                    .data;
                // A slash removes the node's stake; zero both legs so the owner
                // drops out of the staked-overlay set.
                self.supersede(ev.slashed, pos, |mut s| {
                    s.committed = U256::ZERO;
                    s.potential = U256::ZERO;
                    s
                })
            }
            Some(sig) if sig == IStakeRegistry::StakeWithdrawn::SIGNATURE_HASH => {
                let ev = log
                    .log_decode::<IStakeRegistry::StakeWithdrawn>()
                    .map_err(|e| IndexError::apply(INDEXER_NAME, e.to_string()))?
                    .inner
                    .data;
                // A withdrawal empties the node's stake.
                self.supersede(ev.node, pos, |mut s| {
                    s.committed = U256::ZERO;
                    s.potential = U256::ZERO;
                    s
                })
            }
            Some(sig) if sig == OverlayChanged::SIGNATURE_HASH => {
                let ev = log
                    .log_decode::<OverlayChanged>()
                    .map_err(|e| IndexError::apply(INDEXER_NAME, e.to_string()))?
                    .inner
                    .data;
                self.supersede(ev.owner, pos, |mut s| {
                    s.overlay = ev.overlay;
                    s
                })
            }
            // The filter restricts topics to the five above; a log slipping
            // through with an unknown topic0 is ignored rather than erroring,
            // keeping the fold total.
            _ => Ok(()),
        }
    }
}
