//! Redistribution view: `RoundState` is a read-time fold over the Redistribution
//! rows under [`TAG_REDISTRIBUTION`], decoding commit/reveal/anchor events.
//!
//! Grouping folds on the raw `U256` `roundNumber` so two real rounds never
//! collide.

use alloy_primitives::{B256, U256};
use alloy_sol_types::SolEvent;
use vertex_chain_index_framework::{EventKey, StoredEvent, events_of, fold_events};
use vertex_storage::{Database, DatabaseError};

use crate::index::TAG_REDISTRIBUTION;
use crate::index::events;

/// A commit in a round (`Committed`), tagged with its source log position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commit {
    /// The committing log's `(block, log_index)`, the idempotency key.
    pub pos: (u64, u64),
    /// The committing node's overlay address.
    pub overlay: B256,
    /// The committed neighbourhood height.
    pub height: u8,
}

/// A reveal in a round (`Revealed`), tagged with its source log position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reveal {
    /// The revealing log's `(block, log_index)`, the idempotency key.
    pub pos: (u64, u64),
    /// The revealing node's overlay address.
    pub overlay: B256,
    /// The revealing node's stake.
    pub stake: U256,
    /// The revealing node's stake density.
    pub stake_density: U256,
    /// The revealed reserve commitment hash.
    pub reserve_commitment: B256,
    /// The revealed storage depth.
    pub depth: u8,
}

/// The folded state of a single redistribution round.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoundState {
    /// The raw on-chain `roundNumber`, kept full-width.
    pub round: U256,
    /// The reveal anchor, once `CurrentRevealAnchor` is seen.
    pub anchor: Option<B256>,
    /// The commits seen in the round, in position order, deduped by position.
    pub commits: Vec<Commit>,
    /// The reveals seen in the round, in position order, deduped by position.
    pub reveals: Vec<Reveal>,
}

/// Fold the redistribution rows into per-round state.
///
/// [`fold_events`] walks rows in canonical position order, so appends preserve
/// order and a replayed log at a seen position overwrites in place. Decode
/// misses are skipped. Cost is O(events x rounds) per call with no caching or
/// pruning; not for a per-request path.
fn fold_rounds<DB: Database>(db: &DB) -> Result<Vec<RoundState>, DatabaseError> {
    let rounds: Vec<RoundState> = Vec::new();

    // Linear search keeps the full-width `U256` round key (no u64-narrowing map
    // key); the bucket is created on first sight. `get_mut` keeps the fold total
    // (no indexing panic).
    fn round_mut(rounds: &mut Vec<RoundState>, round: U256) -> Option<&mut RoundState> {
        let pos = match rounds.iter().position(|r| r.round == round) {
            Some(pos) => pos,
            None => {
                rounds.push(RoundState {
                    round,
                    ..Default::default()
                });
                rounds.len() - 1
            }
        };
        rounds.get_mut(pos)
    }

    fold_events(db, TAG_REDISTRIBUTION, rounds, |rounds, key, ev| {
        let data = ev.log_data();
        let pos = (key.block, key.log_index);
        if ev.topic0 == events::Committed::SIGNATURE_HASH
            && let Ok(e) = events::Committed::decode_log_data(&data)
            && let Some(round) = round_mut(rounds, e.roundNumber)
        {
            upsert_by_pos(
                &mut round.commits,
                Commit {
                    pos,
                    overlay: e.overlay,
                    height: e.height,
                },
            );
        } else if ev.topic0 == events::Revealed::SIGNATURE_HASH
            && let Ok(e) = events::Revealed::decode_log_data(&data)
            && let Some(round) = round_mut(rounds, e.roundNumber)
        {
            upsert_by_pos(
                &mut round.reveals,
                Reveal {
                    pos,
                    overlay: e.overlay,
                    stake: e.stake,
                    stake_density: e.stakeDensity,
                    reserve_commitment: e.reserveCommitment,
                    depth: e.depth,
                },
            );
        } else if ev.topic0 == events::CurrentRevealAnchor::SIGNATURE_HASH
            && let Ok(e) = events::CurrentRevealAnchor::decode_log_data(&data)
            && let Some(round) = round_mut(rounds, e.roundNumber)
        {
            round.anchor = Some(e.anchor);
        }
    })
}

/// The folded state of round `round_number`, if any of its events were indexed.
///
/// `round_number` widens to `U256` before matching, so no round is narrowed; a
/// caller needing a round beyond `u64::MAX` reads [`rounds`] directly.
pub fn round<DB: Database>(
    db: &DB,
    round_number: u64,
) -> Result<Option<RoundState>, DatabaseError> {
    let target = U256::from(round_number);
    Ok(fold_rounds(db)?.into_iter().find(|r| r.round == target))
}

/// Every round whose events have been indexed, in first-seen order.
pub fn rounds<DB: Database>(db: &DB) -> Result<Vec<RoundState>, DatabaseError> {
    fold_rounds(db)
}

/// The raw position-ordered redistribution event stream (every event, including
/// the round-terminal ones that carry no round number), for a consumer that
/// wants the verbatim log rather than the per-round projection.
pub fn raw_events<DB: Database>(db: &DB) -> Result<Vec<(EventKey, StoredEvent)>, DatabaseError> {
    events_of(db, TAG_REDISTRIBUTION)
}

/// Carries a `(block, log_index)` position used as the idempotency key.
trait HasPos {
    fn pos(&self) -> (u64, u64);
}

impl HasPos for Commit {
    fn pos(&self) -> (u64, u64) {
        self.pos
    }
}

impl HasPos for Reveal {
    fn pos(&self) -> (u64, u64) {
        self.pos
    }
}

/// Replace the item at this log position, or insert it keeping position order.
fn upsert_by_pos<T: HasPos>(items: &mut Vec<T>, incoming: T) {
    match items.binary_search_by(|x| x.pos().cmp(&incoming.pos())) {
        Ok(idx) => {
            if let Some(slot) = items.get_mut(idx) {
                *slot = incoming;
            }
        }
        Err(idx) => items.insert(idx, incoming),
    }
}
