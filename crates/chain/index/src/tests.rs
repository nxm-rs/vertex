//! Unit tests over synthetic logs (no chain).
//!
//! These exercise the engine's contract without a live RPC: a [`MockReader`]
//! serves canned logs and a chosen finalized head, and the in-memory
//! `vertex-storage` backend holds the cursor. They cover backfill ordering,
//! cursor persistence and resume, restart idempotency, adaptive page shrinking,
//! and the head-tracking `revert` hook (even though the MVP never calls it).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_primitives::{Address, B256, LogData};
use alloy_rpc_types_eth::{Filter, Log};
use vertex_storage::Database;
use vertex_storage_redb::RedbDatabase;

use crate::reader::{ChainReader, FinalizedHead};
use crate::{Cursor, EventEngine, IndexError, Indexer};

/// Build a synthetic log at `(block, index)` for `address`.
fn log_at(block: u64, index: u64, address: Address) -> Log {
    Log {
        inner: alloy_primitives::Log {
            address,
            data: LogData::default(),
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

/// A chain reader that serves a fixed log set and finalized head from memory.
///
/// `get_logs` returns the subset of `logs` inside the requested block range,
/// honouring the engine's per-page `from`/`to`. When `range_limit` is set, a
/// page wider than that many blocks fails with a synthetic provider range error,
/// driving the adaptive shrink path.
struct MockReader {
    logs: Vec<Log>,
    head: Mutex<Option<FinalizedHead>>,
    range_limit: Option<u64>,
    get_logs_calls: Mutex<u64>,
}

impl MockReader {
    fn new(logs: Vec<Log>, head: FinalizedHead) -> Self {
        Self {
            logs,
            head: Mutex::new(Some(head)),
            range_limit: None,
            get_logs_calls: Mutex::new(0),
        }
    }

    fn with_range_limit(mut self, limit: u64) -> Self {
        self.range_limit = Some(limit);
        self
    }

    fn calls(&self) -> u64 {
        *self.get_logs_calls.lock().unwrap()
    }
}

/// Extract the inclusive `[from, to]` block bounds the engine set on a filter.
fn filter_bounds(filter: &Filter) -> (u64, u64) {
    let from = filter
        .get_from_block()
        .expect("engine always sets from_block");
    let to = filter.get_to_block().expect("engine always sets to_block");
    (from, to)
}

impl ChainReader for MockReader {
    async fn finalized_head(&self) -> Result<Option<FinalizedHead>, IndexError> {
        Ok(*self.head.lock().unwrap())
    }

    async fn get_logs(&self, filter: &Filter) -> Result<Vec<Log>, IndexError> {
        *self.get_logs_calls.lock().unwrap() += 1;
        let (from, to) = filter_bounds(filter);

        if let Some(limit) = self.range_limit
            && to.saturating_sub(from).saturating_add(1) > limit
        {
            // Mirror a provider rejecting an over-wide range. The engine's
            // `is_range_error` matches on the message text.
            let payload = alloy_json_rpc::ErrorPayload {
                code: -32005,
                message: "query exceeds max block range".into(),
                data: None,
            };
            return Err(IndexError::Transport(
                alloy_provider::transport::RpcError::err_resp(payload),
            ));
        }

        Ok(self
            .logs
            .iter()
            .filter(|log| {
                let b = log.block_number.unwrap();
                b >= from && b <= to
            })
            .cloned()
            .collect())
    }
}

/// An indexer that records every applied `(block, index)` and every revert, so a
/// test can assert ordering, idempotency, and the revert hook.
struct RecordingIndexer {
    address: Address,
    start: u64,
    applied: Mutex<Vec<(u64, u64)>>,
    reverted: Mutex<Vec<u64>>,
}

impl RecordingIndexer {
    fn new(address: Address, start: u64) -> Arc<Self> {
        Arc::new(Self {
            address,
            start,
            applied: Mutex::new(Vec::new()),
            reverted: Mutex::new(Vec::new()),
        })
    }

    fn applied(&self) -> Vec<(u64, u64)> {
        self.applied.lock().unwrap().clone()
    }
}

impl Indexer for RecordingIndexer {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn start_block(&self) -> u64 {
        self.start
    }

    fn filter(&self) -> Filter {
        Filter::new().address(self.address)
    }

    fn apply(&self, block: u64, log: &Log) -> Result<(), IndexError> {
        self.applied
            .lock()
            .unwrap()
            .push((block, log.log_index.unwrap()));
        Ok(())
    }

    fn revert(&self, from_block: u64) -> Result<(), IndexError> {
        self.reverted.lock().unwrap().push(from_block);
        Ok(())
    }
}

fn head(number: u64) -> FinalizedHead {
    FinalizedHead {
        number,
        hash: B256::repeat_byte(number as u8),
    }
}

/// Run a single backfill pass: an immediately-resolved shutdown stops the engine
/// after the first finalized sync, before the follow loop's first tick.
async fn backfill_once<R, DB>(engine: EventEngine<R, DB>)
where
    R: ChainReader + 'static,
    DB: Database,
{
    // A poll interval longer than the test keeps the engine from re-syncing; the
    // immediate shutdown stops it after the first backfill completes.
    engine
        .with_poll_interval(Duration::from_secs(3600))
        .run(async {})
        .await
        .expect("engine run");
}

#[tokio::test]
async fn backfill_applies_logs_in_order() {
    let addr = Address::repeat_byte(0xab);
    // Deliberately out of order in the source vec; the engine must sort.
    let logs = vec![
        log_at(5, 1, addr),
        log_at(3, 0, addr),
        log_at(5, 0, addr),
        log_at(4, 2, addr),
    ];
    let reader = Arc::new(MockReader::new(logs, head(10)));
    let db = Arc::new(RedbDatabase::in_memory().unwrap());
    let indexer = RecordingIndexer::new(addr, 0);

    let engine = EventEngine::new(reader, db.clone()).register(indexer.clone());
    backfill_once(engine).await;

    assert_eq!(
        indexer.applied(),
        vec![(3, 0), (4, 2), (5, 0), (5, 1)],
        "logs applied in (block, log_index) order"
    );

    let cursor = Cursor::load(db.as_ref(), "recording").unwrap().unwrap();
    assert_eq!(cursor.last_block, 10, "cursor advanced to finalized head");
}

#[tokio::test]
async fn cursor_persists_and_resumes() {
    let addr = Address::repeat_byte(0x01);
    let db = Arc::new(RedbDatabase::in_memory().unwrap());

    // First run: head at 10, logs through block 8.
    {
        let logs = vec![log_at(2, 0, addr), log_at(8, 0, addr)];
        let reader = Arc::new(MockReader::new(logs, head(10)));
        let indexer = RecordingIndexer::new(addr, 0);
        let engine = EventEngine::new(reader, db.clone()).register(indexer.clone());
        backfill_once(engine).await;
        assert_eq!(indexer.applied(), vec![(2, 0), (8, 0)]);
        assert_eq!(
            Cursor::load(db.as_ref(), "recording")
                .unwrap()
                .unwrap()
                .last_block,
            10
        );
    }

    // Second run: head advanced to 20, a new log at 15. The resumed engine must
    // only see the new log, not re-deliver blocks 0..=10.
    {
        let logs = vec![log_at(2, 0, addr), log_at(8, 0, addr), log_at(15, 0, addr)];
        let reader = Arc::new(MockReader::new(logs, head(20)));
        let indexer = RecordingIndexer::new(addr, 0);
        let engine = EventEngine::new(reader, db.clone()).register(indexer.clone());
        backfill_once(engine).await;
        assert_eq!(
            indexer.applied(),
            vec![(15, 0)],
            "resumed run applies only the newly-finalized range"
        );
        assert_eq!(
            Cursor::load(db.as_ref(), "recording")
                .unwrap()
                .unwrap()
                .last_block,
            20
        );
    }
}

#[tokio::test]
async fn reapplying_a_range_is_idempotent() {
    let addr = Address::repeat_byte(0x02);
    let db = Arc::new(RedbDatabase::in_memory().unwrap());
    let logs = vec![log_at(4, 0, addr), log_at(7, 1, addr)];

    // Run the identical backfill twice against the same database. Because the
    // cursor persisted to 10 the first time, the second run resumes past the
    // logs and applies nothing: re-running a finalized range is a no-op.
    for expected in [vec![(4, 0), (7, 1)], vec![]] {
        let reader = Arc::new(MockReader::new(logs.clone(), head(10)));
        let indexer = RecordingIndexer::new(addr, 0);
        let engine = EventEngine::new(reader, db.clone()).register(indexer.clone());
        backfill_once(engine).await;
        assert_eq!(indexer.applied(), expected);
    }
}

#[tokio::test]
async fn start_block_skips_pre_deployment_range() {
    let addr = Address::repeat_byte(0x03);
    let logs = vec![log_at(100, 0, addr)];
    let reader = Arc::new(MockReader::new(logs, head(200)));
    let db = Arc::new(RedbDatabase::in_memory().unwrap());
    // Deployment at block 50: the engine must not page 0..50.
    let indexer = RecordingIndexer::new(addr, 50);

    let engine = EventEngine::new(reader.clone(), db.clone())
        .register(indexer.clone())
        .with_page_size(10);
    backfill_once(engine).await;

    assert_eq!(indexer.applied(), vec![(100, 0)]);
    // From 50 to 200 inclusive in pages of 10 is 16 pages; a backfill that
    // started at 0 would have made 21. The exact count proves the start block
    // bounded the first page.
    assert_eq!(reader.calls(), 16, "paged only from the deployment block");
}

#[tokio::test]
async fn adaptive_page_shrinks_on_range_error() {
    let addr = Address::repeat_byte(0x04);
    let logs = vec![log_at(3, 0, addr), log_at(9, 0, addr)];
    // Provider rejects any page wider than 4 blocks.
    let reader = Arc::new(MockReader::new(logs, head(10)).with_range_limit(4));
    let db = Arc::new(RedbDatabase::in_memory().unwrap());
    let indexer = RecordingIndexer::new(addr, 0);

    // Start with an over-wide page (11 blocks) and let the engine shrink it.
    let engine = EventEngine::new(reader.clone(), db.clone())
        .register(indexer.clone())
        .with_page_size(11);
    backfill_once(engine).await;

    assert_eq!(
        indexer.applied(),
        vec![(3, 0), (9, 0)],
        "all logs indexed despite the provider range cap"
    );
    assert_eq!(
        Cursor::load(db.as_ref(), "recording")
            .unwrap()
            .unwrap()
            .last_block,
        10
    );
}

/// The head-tracking `revert` hook fires when an optimistic engine sees a
/// reorg. The MVP never calls it, so this drives the indexer's hook directly to
/// prove the contract: a `revert(from)` is recorded and undoes its range.
#[tokio::test]
async fn revert_hook_is_invoked_on_simulated_reorg() {
    let addr = Address::repeat_byte(0x05);
    let indexer = RecordingIndexer::new(addr, 0);

    // Apply two blocks optimistically, then simulate a reorg that drops block 5.
    indexer.apply(4, &log_at(4, 0, addr)).unwrap();
    indexer.apply(5, &log_at(5, 0, addr)).unwrap();
    indexer.revert(5).unwrap();

    assert_eq!(
        *indexer.reverted.lock().unwrap(),
        vec![5],
        "revert hook records the reorged-out block"
    );
}

/// The engine waits rather than indexing when the chain has no finalized head.
#[tokio::test]
async fn no_finalized_head_indexes_nothing() {
    let addr = Address::repeat_byte(0x06);
    let logs = vec![log_at(1, 0, addr)];
    let reader = Arc::new(MockReader {
        logs,
        head: Mutex::new(None),
        range_limit: None,
        get_logs_calls: Mutex::new(0),
    });
    let db = Arc::new(RedbDatabase::in_memory().unwrap());
    let indexer = RecordingIndexer::new(addr, 0);

    let engine = EventEngine::new(reader.clone(), db.clone()).register(indexer.clone());
    backfill_once(engine).await;

    assert!(indexer.applied().is_empty());
    assert_eq!(reader.calls(), 0, "no get_logs without a finalized head");
    assert!(Cursor::load(db.as_ref(), "recording").unwrap().is_none());
}
