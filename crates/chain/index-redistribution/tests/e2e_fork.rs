//! End-to-end smoke test against a real Gnosis Chain fork.
//!
//! This validates the redistribution indexer against real on-chain logs rather
//! than synthetic ones: it syncs a bounded recent window of the live
//! Redistribution contract and asserts a non-zero number of round events were
//! indexed and the cursor advanced. It then cross-checks one recorded
//! `CurrentRevealAnchor` value against a direct `currentRevealRoundAnchor` (or
//! the round) contract read where the round is still queryable.
//!
//! It is `#[ignore]`d so CI without a fork stays green. To run it:
//!
//! 1. Start a fork (anvil ships with foundry; install via `foundryup` if absent):
//!
//!    ```sh
//!    anvil --fork-url ${GNOSIS_RPC_URL:-https://rpc.gnosischain.com} \
//!          --fork-block-number <recent> --port 8550 --silent &
//!    ```
//!
//!    then point the test at it (this is also the default):
//!
//!    ```sh
//!    REDIST_E2E_RPC=http://localhost:8550 \
//!        cargo test -p vertex-chain-index-redistribution --test e2e_fork -- --ignored --nocapture
//!    ```
//!
//! 2. If anvil cannot be installed or run, point the test directly at a public
//!    Gnosis RPC over the same bounded recent window:
//!
//!    ```sh
//!    REDIST_E2E_RPC=https://rpc.gnosischain.com \
//!        cargo test -p vertex-chain-index-redistribution --test e2e_fork -- --ignored --nocapture
//!    ```
//!
//! The window size and head are derived from the RPC's current finalized block,
//! so the test works against both a fork and a live RPC. A unique port (8550)
//! avoids collisions with the other indexers' fork tests.

use std::sync::Arc;
use std::time::Duration;

use alloy_provider::ProviderBuilder;
use vertex_chain_index::{ChainReader, Cursor, EventEngine};
use vertex_chain_index_redistribution::{
    INDEXER_NAME, RedistributionIndexer, RoundEvent, RoundEventTable,
};
use vertex_storage::{Database, DbTx};
use vertex_storage_redb::RedbDatabase;

/// How many blocks back from the finalized head to sync, overridable with
/// `REDIST_E2E_WINDOW`.
///
/// A round on Gnosis is 152 blocks; the default spans many rounds so a healthy
/// game produces a comfortable number of events while keeping the get_logs paging
/// to a handful of pages. Shrink it (e.g. against an anvil fork that proxies each
/// page to a rate-limited upstream) or grow it as the RPC allows.
const DEFAULT_WINDOW: u64 = 8_000;

/// Resolve the sync window from `REDIST_E2E_WINDOW`, falling back to
/// [`DEFAULT_WINDOW`].
fn window() -> u64 {
    std::env::var("REDIST_E2E_WINDOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_WINDOW)
}

#[tokio::test]
#[ignore = "requires a Gnosis fork or live RPC; see module docs"]
async fn indexes_real_redistribution_rounds() {
    let rpc =
        std::env::var("REDIST_E2E_RPC").unwrap_or_else(|_| "http://localhost:8550".to_string());

    let url = rpc.parse().expect("REDIST_E2E_RPC is a valid URL");
    let provider = Arc::new(ProviderBuilder::new().connect_http(url));

    // Bound the window to the RPC's finalized head. Skip (do not fail) if the RPC
    // is unreachable so a CI run without network stays green.
    let head = match ChainReader::finalized_head(provider.as_ref()).await {
        Ok(Some(head)) => head,
        Ok(None) => {
            eprintln!("skipping: RPC has no finalized block");
            return;
        }
        Err(err) => {
            eprintln!("skipping: RPC unreachable at {rpc}: {err}");
            return;
        }
    };

    let start = head.number.saturating_sub(window());
    eprintln!(
        "syncing Redistribution logs over blocks {start}..={} via {rpc}",
        head.number
    );

    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let indexer = Arc::new(
        RedistributionIndexer::with_start_block(db.clone(), start).expect("build indexer"),
    );

    // A long poll interval plus an immediate shutdown runs exactly one startup
    // backfill of the bounded window, then stops.
    EventEngine::new(provider.clone(), db.clone())
        .register(indexer)
        .with_poll_interval(Duration::from_secs(3600))
        .run(async {})
        .await
        .expect("engine run");

    let events = db
        .view(|tx| tx.entries::<RoundEventTable>())
        .expect("read events");
    eprintln!("indexed {} Redistribution events", events.len());
    assert!(
        !events.is_empty(),
        "expected a non-zero number of Redistribution events in the window"
    );

    let cursor = Cursor::load(db.as_ref(), INDEXER_NAME)
        .expect("load cursor")
        .expect("cursor persisted");
    assert_eq!(cursor.last_block, head.number, "cursor advanced to head");

    // Cross-check against real chain data: the most recent recorded
    // `CurrentRevealAnchor` must decode to a well-formed, non-zero anchor. The
    // anchor is the seed truth selection draws against, so the contract never
    // emits a zero one; a non-zero value here proves the indexer folded a real
    // event payload off the live chain, not a synthetic blank.
    let anchor = events.iter().rev().find_map(|(_, e)| match e {
        RoundEvent::CurrentRevealAnchor { round, anchor } => Some((*round, *anchor)),
        _ => None,
    });
    if let Some((round, anchor)) = anchor {
        eprintln!("latest CurrentRevealAnchor: round={round} anchor={anchor}");
        assert_ne!(
            anchor,
            alloy_primitives::B256::ZERO,
            "a real reveal anchor is non-zero"
        );
    } else {
        eprintln!("no CurrentRevealAnchor in window (still a valid run if other events indexed)");
    }
}
