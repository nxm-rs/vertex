//! Redistribution view: per-round commit/reveal/anchor state and the raw event
//! stream, by lazy fold.
//!
//! The branch split this into a per-round `RoundState` table and a verbatim
//! `RoundEventTable`; here the verbatim half *is* the generic
//! [`EventTable`](crate::store::EventTable) (the `Redistribution` rows), and
//! `RoundState` is a read-time fold: filter the contract's rows, decode
//! `Committed` / `Revealed` / `CurrentRevealAnchor`, group by the decoded
//! `roundNumber`, and dedup commits/reveals by source position.
//!
//! The grouping folds on the RAW `U256` `roundNumber` so two real rounds can
//! never collide. The keyed [`round`] lookup takes a `u64` for caller
//! convenience and widens it to `U256` before matching, so no round number is
//! ever narrowed during indexing (strictly safer than the branch's
//! `saturating_to::<u64>` round key, which could in principle collide two
//! pathological rounds).

use alloy_primitives::{B256, U256};
use alloy_sol_types::SolEvent;
use vertex_storage::{Database, DatabaseError};

use crate::registry::{ContractId, abi};
use crate::store::{EventKey, events_of};

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
    /// The raw on-chain `roundNumber`, kept full-width so a keyed lookup never
    /// collides two real rounds.
    pub round: U256,
    /// The reveal anchor, once `CurrentRevealAnchor` is seen.
    pub anchor: Option<B256>,
    /// The commits seen in the round, in position order, deduped by position.
    pub commits: Vec<Commit>,
    /// The reveals seen in the round, in position order, deduped by position.
    pub reveals: Vec<Reveal>,
}

/// Fold the redistribution rows that carry a `roundNumber` into per-round state,
/// grouped on the raw `U256` round number.
///
/// Rows arrive in canonical position order, so appending commits/reveals
/// preserves order; a replayed log at an already-seen position overwrites in
/// place rather than duplicating.
fn fold_rounds<DB: Database>(db: &DB) -> Result<Vec<RoundState>, DatabaseError> {
    // Grouped on the raw U256 round number; a Vec keyed by linear search keeps
    // the full-width key without a u64-narrowing map key.
    let mut rounds: Vec<RoundState> = Vec::new();

    // Find or create the round bucket for `round`, returning a mutable handle.
    // A `Vec` keyed by linear search keeps the full-width `U256` key (rather
    // than a u64-narrowing map key) so two real rounds never collide. The
    // closure pushes when absent and returns `None` only for an impossible
    // empty-after-push, which the call sites collapse with `?`/`else` rather
    // than panicking, keeping the fold total.
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

    for (key, ev) in events_of(db, ContractId::Redistribution)? {
        let data = ev.log_data();
        let pos = (key.block, key.log_index);
        if ev.topic0 == abi::Committed::SIGNATURE_HASH
            && let Ok(e) = abi::Committed::decode_log_data(&data)
            && let Some(round) = round_mut(&mut rounds, e.roundNumber)
        {
            upsert_commit(
                &mut round.commits,
                Commit {
                    pos,
                    overlay: e.overlay,
                    height: e.height,
                },
            );
        } else if ev.topic0 == abi::Revealed::SIGNATURE_HASH
            && let Ok(e) = abi::Revealed::decode_log_data(&data)
            && let Some(round) = round_mut(&mut rounds, e.roundNumber)
        {
            upsert_reveal(
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
        } else if ev.topic0 == abi::CurrentRevealAnchor::SIGNATURE_HASH
            && let Ok(e) = abi::CurrentRevealAnchor::decode_log_data(&data)
            && let Some(round) = round_mut(&mut rounds, e.roundNumber)
        {
            round.anchor = Some(e.anchor);
        }
    }
    Ok(rounds)
}

/// The folded state of round `round_number`, if any of its events were indexed.
///
/// `round_number` is widened to `U256` before matching, and the fold groups on
/// the raw `U256`, so two distinct full-width rounds are always kept apart in
/// [`rounds`] and only the exact-`U256` match is returned here. For a real round
/// number (`block / blocksPerRound`, far inside `u64`) the `u64` argument is
/// exact; a caller needing a round beyond `u64::MAX` reads [`rounds`] directly.
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
pub fn raw_events<DB: Database>(
    db: &DB,
) -> Result<Vec<(EventKey, crate::store::StoredEvent)>, DatabaseError> {
    events_of(db, ContractId::Redistribution)
}

/// Replace the commit at this log position, or insert it keeping position order.
fn upsert_commit(items: &mut Vec<Commit>, incoming: Commit) {
    match items.binary_search_by(|c| c.pos.cmp(&incoming.pos)) {
        Ok(idx) => {
            if let Some(slot) = items.get_mut(idx) {
                *slot = incoming;
            }
        }
        Err(idx) => items.insert(idx, incoming),
    }
}

/// Replace the reveal at this log position, or insert it keeping position order.
fn upsert_reveal(items: &mut Vec<Reveal>, incoming: Reveal) {
    match items.binary_search_by(|r| r.pos.cmp(&incoming.pos)) {
        Ok(idx) => {
            if let Some(slot) = items.get_mut(idx) {
                *slot = incoming;
            }
        }
        Err(idx) => items.insert(idx, incoming),
    }
}
