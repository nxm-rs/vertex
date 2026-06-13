//! The [`RedistributionIndexer`]: the per-contract fold over Redistribution logs.
//!
//! This is the contract-specific half the generic [`EventEngine`] drives. It
//! declares the contract address, the deployment block, and the `topic0` set of
//! the events it folds, then folds each decoded log into the
//! [`projection`](crate::projection) with a pure, idempotent write.
//!
//! Per the chain-reactions design, `apply` only RECORDS: it writes projection
//! rows and returns. It calls into no domain crate, fires no reaction, and gates
//! no node behaviour. The redistribution game is a clock; consumers read this
//! projection lazily at their own decision point.
//!
//! [`EventEngine`]: vertex_chain_index::EventEngine

use std::sync::Arc;

use alloy_primitives::{Address, address};
use alloy_rpc_types_eth::{Filter, Log};
use alloy_sol_types::SolEvent;
use vertex_chain_index::{IndexError, Indexer};
use vertex_storage::{Database, DbTxMut};

use crate::events::{
    ChunkCount, Committed, CountCommits, CountReveals, CurrentRevealAnchor, Revealed,
    TruthSelected, WinnerSelected,
};
use crate::projection::{
    Commit, LogKey, RedistributionTables, Reveal, RoundEvent, RoundEventTable, RoundKey,
    RoundState, RoundTable,
};

/// The Redistribution contract on Gnosis Chain (id 100).
pub const REDISTRIBUTION_ADDRESS: Address = address!("5069cdfB3D9E56d23B1cAeE83CE6109A7E4fd62d");

/// The Redistribution contract deployment block on Gnosis Chain.
pub const REDISTRIBUTION_DEPLOYMENT_BLOCK: u64 = 41_105_199;

/// The indexer name, used as the engine's cursor key and metric label.
pub const INDEXER_NAME: &str = "redistribution";

/// Folds Redistribution game logs into the [`crate::projection`].
///
/// Construct with [`new`](RedistributionIndexer::new) over a `vertex-storage`
/// [`Database`], then register it with a
/// [`EventEngine`](vertex_chain_index::EventEngine). The engine delivers each
/// matching log to [`apply`](Indexer::apply) in canonical order; `apply` decodes
/// it and writes the corresponding projection rows in one transaction.
pub struct RedistributionIndexer<DB> {
    db: Arc<DB>,
    start_block: u64,
}

impl<DB: Database> RedistributionIndexer<DB> {
    /// Build the indexer over the database holding the projection tables.
    ///
    /// Backfills from the contract's deployment block. Initializes the projection
    /// tables so the first `apply` has somewhere to write; the engine separately
    /// initializes its own cursor table. Takes the database by `Arc` so the same
    /// handle the [`EventEngine`](vertex_chain_index::EventEngine) drives can be
    /// shared.
    pub fn new(db: Arc<DB>) -> Result<Self, IndexError> {
        Self::with_start_block(db, REDISTRIBUTION_DEPLOYMENT_BLOCK)
    }

    /// Build the indexer with an explicit backfill start block.
    ///
    /// Production uses [`new`](Self::new) (the deployment block); tests use this
    /// to bound the backfill to a recent window. A start block below the
    /// deployment block has no effect, since the engine never pages before the
    /// contract existed.
    pub fn with_start_block(db: Arc<DB>, start_block: u64) -> Result<Self, IndexError> {
        RedistributionTables::init(db.as_ref())?;
        Ok(Self { db, start_block })
    }

    /// Decode `log` against `E` or map the decode failure to an apply error.
    fn decode<E: SolEvent>(log: &Log) -> Result<E, IndexError> {
        log.log_decode::<E>()
            .map(|decoded| decoded.inner.data)
            .map_err(|e| IndexError::apply(INDEXER_NAME, e.to_string()))
    }

    /// Fold a round-carrying event into [`RoundTable`], creating the round row on
    /// first sight. `mutate` applies the event to the loaded-or-default state.
    fn upsert_round<TX, F>(tx: &TX, round: RoundKey, mutate: F) -> Result<(), IndexError>
    where
        TX: DbTxMut,
        F: FnOnce(&mut RoundState),
    {
        let mut state = tx.get::<RoundTable>(round)?.unwrap_or_default();
        mutate(&mut state);
        tx.put::<RoundTable>(round, state)?;
        Ok(())
    }
}

impl<DB: Database> Indexer for RedistributionIndexer<DB> {
    fn name(&self) -> &'static str {
        // `INDEXER_NAME` is the same string; `name` returns the `'static` form
        // the trait requires.
        "redistribution"
    }

    fn start_block(&self) -> u64 {
        self.start_block
    }

    fn filter(&self) -> Filter {
        Filter::new()
            .address(REDISTRIBUTION_ADDRESS)
            .event_signature(vec![
                Committed::SIGNATURE_HASH,
                Revealed::SIGNATURE_HASH,
                CurrentRevealAnchor::SIGNATURE_HASH,
                TruthSelected::SIGNATURE_HASH,
                WinnerSelected::SIGNATURE_HASH,
                CountCommits::SIGNATURE_HASH,
                CountReveals::SIGNATURE_HASH,
                ChunkCount::SIGNATURE_HASH,
            ])
    }

    fn apply(&self, block: u64, log: &Log) -> Result<(), IndexError> {
        let log_index = log
            .log_index
            .ok_or(IndexError::MalformedLog { field: "log_index" })?;
        let pos = LogKey {
            block_number: block,
            log_index,
        };

        let topic0 = log
            .topics()
            .first()
            .copied()
            .ok_or(IndexError::apply(INDEXER_NAME, "log has no topic0"))?;

        let tx = self.db.as_ref().tx_mut()?;

        // Record the raw event verbatim, keyed by log position. This is the
        // fully-idempotent half: a replayed log overwrites its own row.
        let event = match topic0 {
            Committed::SIGNATURE_HASH => {
                let e = Self::decode::<Committed>(log)?;
                let round = round_key(e.roundNumber);
                Self::upsert_round(&tx, round, |state| {
                    state.upsert_commit(Commit {
                        pos,
                        overlay: e.overlay,
                        height: e.height,
                    });
                })?;
                RoundEvent::Committed {
                    round: e.roundNumber,
                    overlay: e.overlay,
                    height: e.height,
                }
            }
            Revealed::SIGNATURE_HASH => {
                let e = Self::decode::<Revealed>(log)?;
                let round = round_key(e.roundNumber);
                Self::upsert_round(&tx, round, |state| {
                    state.upsert_reveal(Reveal {
                        pos,
                        overlay: e.overlay,
                        stake: e.stake,
                        stake_density: e.stakeDensity,
                        reserve_commitment: e.reserveCommitment,
                        depth: e.depth,
                    });
                })?;
                RoundEvent::Revealed {
                    round: e.roundNumber,
                    overlay: e.overlay,
                    stake: e.stake,
                    stake_density: e.stakeDensity,
                    reserve_commitment: e.reserveCommitment,
                    depth: e.depth,
                }
            }
            CurrentRevealAnchor::SIGNATURE_HASH => {
                let e = Self::decode::<CurrentRevealAnchor>(log)?;
                let round = round_key(e.roundNumber);
                Self::upsert_round(&tx, round, |state| {
                    state.anchor = Some(e.anchor);
                })?;
                RoundEvent::CurrentRevealAnchor {
                    round: e.roundNumber,
                    anchor: e.anchor,
                }
            }
            TruthSelected::SIGNATURE_HASH => {
                let e = Self::decode::<TruthSelected>(log)?;
                RoundEvent::TruthSelected {
                    hash: e.hash,
                    depth: e.depth,
                }
            }
            WinnerSelected::SIGNATURE_HASH => {
                let e = Self::decode::<WinnerSelected>(log)?;
                RoundEvent::WinnerSelected {
                    overlay: e.winner.overlay,
                    owner: e.winner.owner,
                    depth: e.winner.depth,
                    stake: e.winner.stake,
                    stake_density: e.winner.stakeDensity,
                    hash: e.winner.hash,
                }
            }
            CountCommits::SIGNATURE_HASH => {
                let e = Self::decode::<CountCommits>(log)?;
                RoundEvent::CountCommits { count: e._count }
            }
            CountReveals::SIGNATURE_HASH => {
                let e = Self::decode::<CountReveals>(log)?;
                RoundEvent::CountReveals { count: e._count }
            }
            ChunkCount::SIGNATURE_HASH => {
                let e = Self::decode::<ChunkCount>(log)?;
                RoundEvent::ChunkCount {
                    valid_chunk_count: e.validChunkCount,
                }
            }
            other => {
                return Err(IndexError::apply(
                    INDEXER_NAME,
                    format!("unexpected topic0 {other}"),
                ));
            }
        };

        tx.put::<RoundEventTable>(pos, event)?;
        tx.commit()?;
        Ok(())
    }
}

/// Narrow an on-chain `uint256` round number into the [`RoundKey`] `u64`.
///
/// `roundNumber` is `block / blocksPerRound`, far inside `u64`. A pathological
/// value beyond `u64::MAX` saturates rather than panicking; the raw `uint256` is
/// always preserved verbatim in the recorded [`RoundEvent`].
fn round_key(round: alloy_primitives::U256) -> RoundKey {
    RoundKey(round.saturating_to::<u64>())
}
