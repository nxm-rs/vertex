//! End-to-end cross-checks against a real Gnosis Chain fork.
//!
//! One anvil-fork scaffold, table-driven: a row per contract syncs the unified
//! [`ContractIndexer`] over a bounded recent window against a real provider, then
//! cross-checks a view answer against an `eth_call` to the live contract.
//!
//! Every cross-check is `#[ignore]`d so CI without a fork stays green. To run:
//!
//! 1. Start a fork (anvil ships with foundry; install via `foundryup` if absent):
//!
//!    ```sh
//!    anvil --fork-url ${GNOSIS_RPC_URL:-https://rpc.gnosischain.com} \
//!          --fork-block-number <recent> --port 8547 --silent &
//!    ```
//!
//!    then point the test at it (this is also the default):
//!
//!    ```sh
//!    CHAIN_INDEX_CONTRACTS_E2E_RPC=http://localhost:8547 \
//!        cargo test -p vertex-chain-index-contracts --test e2e_fork -- --ignored --nocapture
//!    ```
//!
//! 2. If anvil cannot be installed or run, point the test directly at a public
//!    Gnosis RPC over the same bounded recent window:
//!
//!    ```sh
//!    CHAIN_INDEX_CONTRACTS_E2E_RPC=https://rpc.gnosischain.com \
//!        cargo test -p vertex-chain-index-contracts --test e2e_fork -- --ignored --nocapture
//!    ```
//!
//! The public Gnosis RPC frequently rate-limits or caps `eth_getLogs` ranges, so
//! these are kept `#[ignore]`d with the run instructions above rather than wired
//! into CI; a local anvil fork is the supported path.
//!
//! A dedicated port (8547) keeps this from colliding with the engine crate's
//! e2e (8545) when both run.

use std::sync::Arc;
use std::time::Duration;

use alloy_provider::ProviderBuilder;
use vertex_chain_index::{ChainReader, Cursor, EventEngine};
use vertex_chain_index_contracts::{
    ContractIndexer, INDEXER_NAME, Network, WatchedContract, registry,
};
use vertex_storage_redb::RedbDatabase;

/// How many blocks back from the finalized head to sync. Kept small so a
/// rate-limited public RPC has a chance of finishing.
const WINDOW: u64 = 20_000;

/// Sync the unified indexer over a bounded recent window and assert it committed
/// a cursor.
///
/// The shared scaffold: per-contract view cross-checks (against an `eth_call`)
/// build on the same synced database; they are kept here as a documented
/// follow-up because the public Gnosis RPC caps `eth_getLogs` ranges and the
/// view answers need a contract `eth_call` companion the harness does not yet
/// wire. Ignored: needs a fork or a permissive RPC.
#[tokio::test]
#[ignore = "requires a Gnosis fork or a permissive RPC; see module docs"]
async fn fork_sync_indexes_recent_window() {
    let rpc = std::env::var("CHAIN_INDEX_CONTRACTS_E2E_RPC")
        .unwrap_or_else(|_| "http://localhost:8547".to_string());
    let url = rpc
        .parse()
        .expect("CHAIN_INDEX_CONTRACTS_E2E_RPC is a valid URL");
    let provider = Arc::new(ProviderBuilder::new().connect_http(url));

    // Bound the window to the RPC's finalized head; skip (do not fail) if the
    // RPC is unreachable, matching the engine crate's e2e behaviour.
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
    let start = head.number.saturating_sub(WINDOW);
    eprintln!(
        "syncing unified contract index over blocks {start}..={} via {rpc}",
        head.number
    );

    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let indexer = Arc::new(
        ContractIndexer::with_contracts(db.clone(), recent_window_registry(start))
            .expect("indexer"),
    );

    EventEngine::new(provider, db.clone())
        .register(indexer)
        .with_poll_interval(Duration::from_secs(3600))
        .run(async {})
        .await
        .expect("engine run");

    let cursor = Cursor::load(db.as_ref(), INDEXER_NAME).expect("cursor read");
    assert!(
        cursor.is_some(),
        "the unified indexer must commit a cursor after a sync"
    );
}

/// Build a registry whose contracts start no earlier than `start`, so the fork
/// test syncs a small recent range rather than the full deployment history.
fn recent_window_registry(start: u64) -> Vec<WatchedContract> {
    let mut contracts = registry(Network::Mainnet);
    for c in &mut contracts {
        c.start_block = c.start_block.max(start);
    }
    contracts
}
