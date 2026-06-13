//! The `vertex-storage` projection the indexer folds Redistribution logs into.
//!
//! Two tables, both written by a pure, idempotent fold (see
//! [`CHAIN_REACTIONS_DESIGN.md`]): re-applying a finalized log re-writes the same
//! row to the same value, never accumulates, and never triggers a side effect.
//!
//! - [`RoundTable`] is the headline "per-round game log", keyed by the on-chain
//!   `roundNumber`. The events that carry a round number ([`Committed`],
//!   [`Revealed`], [`CurrentRevealAnchor`]) fold into the round's [`RoundState`]
//!   with last-write-wins per field and monotonic commit/reveal counters keyed by
//!   the log position, so a replay of the same logs reproduces the same counts.
//! - [`RoundEventTable`] is the raw event log, keyed by `(block_number,
//!   log_index)`. Every Redistribution event the filter selects, including the
//!   round-terminal ones that do not carry a round number in their payload
//!   ([`TruthSelected`], [`WinnerSelected`], [`ChunkCount`], [`CountCommits`],
//!   [`CountReveals`]), lands here verbatim. The log position is the natural
//!   idempotency key: the same finalized log always writes the same row.
//!
//! This crate only RECORDS. The redistribution game is a clock, not a reaction
//! source; nothing here gates node behaviour. Consumers query the projection
//! lazily at their own decision point.
//!
//! [`CHAIN_REACTIONS_DESIGN.md`]: the chain-reactions design note.
//! [`Committed`]: crate::events::Committed
//! [`Revealed`]: crate::events::Revealed
//! [`CurrentRevealAnchor`]: crate::events::CurrentRevealAnchor
//! [`TruthSelected`]: crate::events::TruthSelected
//! [`WinnerSelected`]: crate::events::WinnerSelected
//! [`ChunkCount`]: crate::events::ChunkCount
//! [`CountCommits`]: crate::events::CountCommits
//! [`CountReveals`]: crate::events::CountReveals

use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};
use vertex_storage::{Database, DatabaseError, Decode, Encode, Table};

// The per-round game state, keyed by the on-chain `roundNumber`.
vertex_storage::table!(
    pub RoundTable,
    "redistribution_rounds",
    RoundKey,
    RoundState
);

// The raw Redistribution event log, keyed by `(block_number, log_index)`.
vertex_storage::table!(
    pub RoundEventTable,
    "redistribution_events",
    LogKey,
    RoundEvent
);

/// The set of tables this indexer persists, for one-shot initialization.
pub struct RedistributionTables;

impl vertex_storage::Tables for RedistributionTables {
    const NAMES: &'static [&'static str] = &[RoundTable::NAME, RoundEventTable::NAME];
}

impl RedistributionTables {
    /// Create the projection tables if they do not yet exist.
    pub fn init<DB: Database>(db: &DB) -> Result<(), DatabaseError> {
        <Self as vertex_storage::Tables>::init(db)
    }
}

/// The [`RoundTable`] key: an on-chain `roundNumber`.
///
/// A newtype over `u64` encoded big-endian so the table iterates in ascending
/// round order. `roundNumber` is `uint256` on-chain but is `block /
/// blocksPerRound`, far inside `u64`, so the narrowing is lossless in practice;
/// the raw `uint256` is preserved verbatim in [`RoundEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoundKey(pub u64);

impl Encode for RoundKey {
    type Encoded = [u8; 8];

    fn encode(self) -> Self::Encoded {
        self.0.to_be_bytes()
    }
}

impl Decode for RoundKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 8] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(u64::from_be_bytes(bytes)))
    }
}

/// The [`RoundEventTable`] key: a log's canonical `(block_number, log_index)`
/// position.
///
/// Encoded as the two big-endian `u64`s concatenated so the table iterates in
/// chain order. The log position is unique and immutable for a finalized log, so
/// it is the natural idempotency key: re-applying the same log overwrites its own
/// row with identical bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LogKey {
    /// The block the log was emitted in.
    pub block_number: u64,
    /// The log's index within that block.
    pub log_index: u64,
}

impl Encode for LogKey {
    type Encoded = [u8; 16];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&self.block_number.to_be_bytes());
        out[8..].copy_from_slice(&self.log_index.to_be_bytes());
        out
    }
}

impl Decode for LogKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 16] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let mut b = [0u8; 8];
        b.copy_from_slice(&bytes[..8]);
        let block_number = u64::from_be_bytes(b);
        b.copy_from_slice(&bytes[8..]);
        let log_index = u64::from_be_bytes(b);
        Ok(Self {
            block_number,
            log_index,
        })
    }
}

/// The folded state of a single redistribution round.
///
/// Each field is written last-write-wins by its source event, so re-applying the
/// round's logs reproduces the same row. The commit and reveal sets are keyed by
/// log position, not appended, so a replay never double-counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundState {
    /// The reveal anchor for the round, once `CurrentRevealAnchor` is seen.
    pub anchor: Option<B256>,
    /// The commits seen in the round, keyed by the committing log's position so
    /// a replay overwrites in place rather than appending.
    pub commits: Vec<Commit>,
    /// The reveals seen in the round, keyed by the revealing log's position.
    pub reveals: Vec<Reveal>,
}

impl RoundState {
    /// Fold a `Committed` log in idempotently: replace the entry at this log
    /// position if it already exists, otherwise insert keeping position order.
    pub fn upsert_commit(&mut self, commit: Commit) {
        upsert_by_pos(&mut self.commits, commit, |c| c.pos);
    }

    /// Fold a `Revealed` log in idempotently, the same way as a commit.
    pub fn upsert_reveal(&mut self, reveal: Reveal) {
        upsert_by_pos(&mut self.reveals, reveal, |r| r.pos);
    }
}

/// A commit in a round (`Committed`), tagged with its source log position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    /// The source log's `(block_number, log_index)`, the idempotency key.
    pub pos: LogKey,
    /// The committing node's overlay address.
    pub overlay: B256,
    /// The committed neighbourhood height.
    pub height: u8,
}

/// A reveal in a round (`Revealed`), tagged with its source log position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reveal {
    /// The source log's `(block_number, log_index)`, the idempotency key.
    pub pos: LogKey,
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

/// One raw Redistribution event, recorded verbatim in [`RoundEventTable`].
///
/// The enum carries the decoded payload of every event the indexer selects. The
/// round-carrying variants also drive [`RoundTable`]; the round-terminal variants
/// are recorded here only, since their on-chain payload does not name a round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RoundEvent {
    /// `Committed(roundNumber, overlay, height)`.
    Committed {
        /// The raw on-chain `roundNumber`.
        round: U256,
        /// The committing node's overlay address.
        overlay: B256,
        /// The committed neighbourhood height.
        height: u8,
    },
    /// `Revealed(roundNumber, overlay, stake, stakeDensity, reserveCommitment, depth)`.
    Revealed {
        /// The raw on-chain `roundNumber`.
        round: U256,
        /// The revealing node's overlay address.
        overlay: B256,
        /// The revealing node's stake.
        stake: U256,
        /// The revealing node's stake density.
        stake_density: U256,
        /// The revealed reserve commitment hash.
        reserve_commitment: B256,
        /// The revealed storage depth.
        depth: u8,
    },
    /// `CurrentRevealAnchor(roundNumber, anchor)`.
    CurrentRevealAnchor {
        /// The raw on-chain `roundNumber`.
        round: U256,
        /// The reveal anchor seed.
        anchor: B256,
    },
    /// `TruthSelected(hash, depth)`.
    TruthSelected {
        /// The agreed reserve hash.
        hash: B256,
        /// The agreed storage depth.
        depth: u8,
    },
    /// `WinnerSelected(winner)`.
    WinnerSelected {
        /// The winning node's overlay address.
        overlay: B256,
        /// The winning node's owner address.
        owner: Address,
        /// The winning reveal's storage depth.
        depth: u8,
        /// The winning node's stake.
        stake: U256,
        /// The winning node's stake density.
        stake_density: U256,
        /// The winning reveal's reserve hash.
        hash: B256,
    },
    /// `CountCommits(_count)`.
    CountCommits {
        /// The number of commits counted.
        count: U256,
    },
    /// `CountReveals(_count)`.
    CountReveals {
        /// The number of reveals counted.
        count: U256,
    },
    /// `ChunkCount(validChunkCount)`.
    ChunkCount {
        /// The valid chunk count the round priced against.
        valid_chunk_count: U256,
    },
}

/// Replace the element whose position matches `incoming`, or insert it keeping
/// the vector sorted by log position. This is the idempotency primitive: a
/// re-applied log overwrites its own slot instead of appending a duplicate.
fn upsert_by_pos<T, F>(items: &mut Vec<T>, incoming: T, pos: F)
where
    F: Fn(&T) -> LogKey,
{
    let key = pos(&incoming);
    match items.binary_search_by(|item| pos(item).cmp(&key)) {
        Ok(idx) => {
            if let Some(slot) = items.get_mut(idx) {
                *slot = incoming;
            }
        }
        Err(idx) => items.insert(idx, incoming),
    }
}
