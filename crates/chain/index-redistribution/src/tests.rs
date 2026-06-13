//! Unit tests over synthetic logs: each event decodes and folds into the
//! projection correctly, and re-applying a log is idempotent.
//!
//! No chain here. We build a [`Log`] for each event by ABI-encoding it through
//! the same `sol!` bindings the indexer decodes with, drive
//! [`Indexer::apply`](vertex_chain_index::Indexer::apply) directly, and read the
//! projection back from an in-memory `vertex-storage` backend.

use alloy_primitives::{Address, B256, U256, address};
use alloy_rpc_types_eth::Log;
use alloy_sol_types::SolEvent;
use vertex_chain_index::Indexer;
use vertex_storage::{Database, DbTx};

use crate::RedistributionIndexer;
use crate::events::{
    ChunkCount, Committed, CountCommits, CountReveals, CurrentRevealAnchor, Reveal as SolReveal,
    Revealed, TruthSelected, WinnerSelected,
};
use crate::indexer::REDISTRIBUTION_ADDRESS;
use crate::projection::{LogKey, RoundEvent, RoundEventTable, RoundKey, RoundTable};

type Db = vertex_storage_redb::RedbDatabase;

/// Build a `Log` from an ABI-encoded event, placed at `(block, index)` on the
/// Redistribution contract address.
fn log_for<E: SolEvent>(event: &E, block: u64, index: u64) -> Log {
    let data = event.encode_log_data();
    Log {
        inner: alloy_primitives::Log {
            address: REDISTRIBUTION_ADDRESS,
            data,
        },
        block_hash: Some(B256::repeat_byte(block as u8)),
        block_number: Some(block),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: Some(index),
        removed: false,
    }
}

fn new_indexer() -> (RedistributionIndexer<Db>, std::sync::Arc<Db>) {
    let db = std::sync::Arc::new(Db::in_memory().expect("in-memory db"));
    let indexer = RedistributionIndexer::new(db.clone()).expect("init indexer");
    (indexer, db)
}

const OVERLAY: B256 = B256::repeat_byte(0xaa);
const OWNER: Address = address!("00000000000000000000000000000000000000bb");

#[test]
fn committed_folds_into_round() {
    let (indexer, db) = new_indexer();
    let event = Committed {
        roundNumber: U256::from(7u64),
        overlay: OVERLAY,
        height: 4,
    };
    let log = log_for(&event, 100, 0);
    indexer.apply(100, &log).expect("apply");

    let state = db
        .view(|tx| tx.get::<RoundTable>(RoundKey(7)))
        .unwrap()
        .expect("round row");
    assert_eq!(state.commits.len(), 1);
    let commit = state.commits.first().unwrap();
    assert_eq!(commit.overlay, OVERLAY);
    assert_eq!(commit.height, 4);
    assert_eq!(
        commit.pos,
        LogKey {
            block_number: 100,
            log_index: 0
        }
    );

    // The raw event log carries the verbatim payload.
    let raw = db
        .view(|tx| {
            tx.get::<RoundEventTable>(LogKey {
                block_number: 100,
                log_index: 0,
            })
        })
        .unwrap()
        .expect("raw event row");
    assert!(matches!(
        raw,
        RoundEvent::Committed { round, height, .. } if round == U256::from(7u64) && height == 4
    ));
}

#[test]
fn revealed_folds_into_round() {
    let (indexer, db) = new_indexer();
    let event = Revealed {
        roundNumber: U256::from(7u64),
        overlay: OVERLAY,
        stake: U256::from(1000u64),
        stakeDensity: U256::from(2000u64),
        reserveCommitment: B256::repeat_byte(0xcc),
        depth: 12,
    };
    indexer.apply(101, &log_for(&event, 101, 3)).expect("apply");

    let state = db
        .view(|tx| tx.get::<RoundTable>(RoundKey(7)))
        .unwrap()
        .expect("round row");
    assert_eq!(state.reveals.len(), 1);
    let reveal = state.reveals.first().unwrap();
    assert_eq!(reveal.stake, U256::from(1000u64));
    assert_eq!(reveal.stake_density, U256::from(2000u64));
    assert_eq!(reveal.depth, 12);
}

#[test]
fn anchor_folds_into_round() {
    let (indexer, db) = new_indexer();
    let anchor = B256::repeat_byte(0xde);
    let event = CurrentRevealAnchor {
        roundNumber: U256::from(9u64),
        anchor,
    };
    indexer.apply(200, &log_for(&event, 200, 0)).expect("apply");

    let state = db
        .view(|tx| tx.get::<RoundTable>(RoundKey(9)))
        .unwrap()
        .expect("round row");
    assert_eq!(state.anchor, Some(anchor));
}

#[test]
fn round_terminal_events_record_into_event_log() {
    let (indexer, db) = new_indexer();

    let truth = TruthSelected {
        hash: B256::repeat_byte(0x11),
        depth: 8,
    };
    indexer.apply(300, &log_for(&truth, 300, 0)).expect("apply");

    let winner = WinnerSelected {
        winner: SolReveal {
            overlay: OVERLAY,
            owner: OWNER,
            depth: 8,
            stake: U256::from(5u64),
            stakeDensity: U256::from(6u64),
            hash: B256::repeat_byte(0x22),
        },
    };
    indexer
        .apply(300, &log_for(&winner, 300, 1))
        .expect("apply");

    let commits = CountCommits {
        _count: U256::from(3u64),
    };
    indexer
        .apply(300, &log_for(&commits, 300, 2))
        .expect("apply");

    let reveals = CountReveals {
        _count: U256::from(2u64),
    };
    indexer
        .apply(300, &log_for(&reveals, 300, 3))
        .expect("apply");

    let chunks = ChunkCount {
        validChunkCount: U256::from(42u64),
    };
    indexer
        .apply(300, &log_for(&chunks, 300, 4))
        .expect("apply");

    let events = db.view(|tx| tx.entries::<RoundEventTable>()).unwrap();
    assert_eq!(events.len(), 5, "all five terminal events recorded");

    let get = |idx: u64| {
        db.view(|tx| {
            tx.get::<RoundEventTable>(LogKey {
                block_number: 300,
                log_index: idx,
            })
        })
        .unwrap()
        .unwrap()
    };
    assert!(matches!(get(0), RoundEvent::TruthSelected { depth: 8, .. }));
    assert!(matches!(
        get(1),
        RoundEvent::WinnerSelected { owner, .. } if owner == OWNER
    ));
    assert!(matches!(
        get(2),
        RoundEvent::CountCommits { count } if count == U256::from(3u64)
    ));
    assert!(matches!(
        get(3),
        RoundEvent::CountReveals { count } if count == U256::from(2u64)
    ));
    assert!(matches!(
        get(4),
        RoundEvent::ChunkCount { valid_chunk_count } if valid_chunk_count == U256::from(42u64)
    ));
}

#[test]
fn reapplying_a_commit_is_idempotent() {
    let (indexer, db) = new_indexer();
    let event = Committed {
        roundNumber: U256::from(7u64),
        overlay: OVERLAY,
        height: 4,
    };
    let log = log_for(&event, 100, 0);

    // Apply the same finalized log twice; the round must hold exactly one commit.
    indexer.apply(100, &log).expect("first apply");
    let after_first = db.view(|tx| tx.get::<RoundTable>(RoundKey(7))).unwrap();
    indexer.apply(100, &log).expect("second apply");
    let after_second = db.view(|tx| tx.get::<RoundTable>(RoundKey(7))).unwrap();

    assert_eq!(
        after_first, after_second,
        "re-applying a log re-writes the same row"
    );
    assert_eq!(
        after_second.unwrap().commits.len(),
        1,
        "a replay never double-counts a commit"
    );

    // The raw event table is keyed by log position, so it also holds one row.
    let count = db.view(|tx| tx.count::<RoundEventTable>()).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn distinct_commits_in_a_round_accumulate() {
    let (indexer, db) = new_indexer();
    let a = Committed {
        roundNumber: U256::from(7u64),
        overlay: B256::repeat_byte(0x01),
        height: 4,
    };
    let b = Committed {
        roundNumber: U256::from(7u64),
        overlay: B256::repeat_byte(0x02),
        height: 5,
    };
    indexer.apply(100, &log_for(&a, 100, 0)).expect("apply a");
    indexer.apply(100, &log_for(&b, 100, 1)).expect("apply b");

    let state = db
        .view(|tx| tx.get::<RoundTable>(RoundKey(7)))
        .unwrap()
        .expect("round row");
    assert_eq!(state.commits.len(), 2, "two distinct commits in the round");

    // Replaying both is still idempotent.
    indexer.apply(100, &log_for(&a, 100, 0)).expect("replay a");
    indexer.apply(100, &log_for(&b, 100, 1)).expect("replay b");
    let state = db
        .view(|tx| tx.get::<RoundTable>(RoundKey(7)))
        .unwrap()
        .unwrap();
    assert_eq!(state.commits.len(), 2, "replay does not duplicate");
}

#[test]
fn filter_selects_the_contract_and_all_events() {
    let (indexer, _db) = new_indexer();
    let filter = indexer.filter();
    // The address constraint is the contract.
    let addrs: Vec<_> = filter.address.iter().collect();
    assert!(addrs.contains(&&REDISTRIBUTION_ADDRESS));

    // topic0 carries all eight event signatures.
    let topic0 = filter.topics[0].iter().collect::<Vec<_>>();
    for sig in [
        Committed::SIGNATURE_HASH,
        Revealed::SIGNATURE_HASH,
        CurrentRevealAnchor::SIGNATURE_HASH,
        TruthSelected::SIGNATURE_HASH,
        WinnerSelected::SIGNATURE_HASH,
        CountCommits::SIGNATURE_HASH,
        CountReveals::SIGNATURE_HASH,
        ChunkCount::SIGNATURE_HASH,
    ] {
        assert!(topic0.contains(&&sig), "topic0 set includes every event");
    }
}
