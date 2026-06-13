//! End-to-end smoke test against a real Gnosis Chain fork.
//!
//! This validates the engine against real on-chain logs rather than synthetic
//! ones: it registers a trivial indexer for the BZZ token `Transfer` event, syncs
//! a bounded recent window, and asserts a non-zero number of transfers were
//! indexed and the cursor advanced.
//!
//! It is `#[ignore]`d so CI without a fork stays green. To run it:
//!
//! 1. Start a fork (anvil ships with foundry; install via `foundryup` if absent):
//!
//!    ```sh
//!    anvil --fork-url ${GNOSIS_RPC_URL:-https://rpc.gnosischain.com} \
//!          --fork-block-number <recent> --port 8545 --silent &
//!    ```
//!
//!    then point the test at it (this is also the default):
//!
//!    ```sh
//!    CHAIN_INDEX_E2E_RPC=http://localhost:8545 \
//!        cargo test -p vertex-chain-index --test e2e_fork -- --ignored --nocapture
//!    ```
//!
//! 2. If anvil cannot be installed or run, point the test directly at a public
//!    Gnosis RPC over the same bounded recent window:
//!
//!    ```sh
//!    CHAIN_INDEX_E2E_RPC=https://rpc.gnosischain.com \
//!        cargo test -p vertex-chain-index --test e2e_fork -- --ignored --nocapture
//!    ```
//!
//! The window size and head are derived from the RPC's current finalized block,
//! so the test works against both a fork and a live RPC.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use alloy_primitives::{Address, address};
use alloy_provider::ProviderBuilder;
use alloy_rpc_types_eth::{Filter, Log};
use alloy_sol_types::{SolEvent, sol};
use vertex_chain_index::{ChainReader, Cursor, EventEngine, IndexError, Indexer};
use vertex_storage_redb::RedbDatabase;

/// The BZZ token on Gnosis Chain.
const BZZ_TOKEN: Address = address!("dBF3Ea6F5beE45c02255B2c26a16F300502F68da");

/// How many blocks back from the finalized head to sync.
const WINDOW: u64 = 50_000;

sol! {
    /// The ERC-20 transfer event, used only for its `topic0` signature here.
    #[allow(missing_docs)]
    event Transfer(address indexed from, address indexed to, uint256 value);
}

/// A trivial indexer that counts BZZ `Transfer` logs.
struct TransferCounter {
    start: u64,
    count: Arc<AtomicU64>,
}

impl Indexer for TransferCounter {
    fn name(&self) -> &'static str {
        "bzz_transfers"
    }

    fn start_block(&self) -> u64 {
        self.start
    }

    fn filter(&self) -> Filter {
        Filter::new()
            .address(BZZ_TOKEN)
            .event_signature(Transfer::SIGNATURE_HASH)
    }

    fn apply(&self, _block: u64, log: &Log) -> Result<(), IndexError> {
        // Decode to prove the log is a real, well-formed Transfer.
        log.log_decode::<Transfer>()
            .map_err(|e| IndexError::apply(self.name(), e.to_string()))?;
        self.count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires a Gnosis fork or live RPC; see module docs"]
async fn indexes_real_bzz_transfers() {
    let rpc = std::env::var("CHAIN_INDEX_E2E_RPC")
        .unwrap_or_else(|_| "http://localhost:8545".to_string());

    let url = rpc.parse().expect("CHAIN_INDEX_E2E_RPC is a valid URL");
    let provider = Arc::new(ProviderBuilder::new().connect_http(url));

    // Bound the window to the RPC's finalized head so the test is fast on a fork
    // and on a live RPC alike. Skip (do not fail) if the RPC is unreachable.
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
        "syncing BZZ Transfer logs over blocks {start}..={} via {rpc}",
        head.number
    );

    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let count = Arc::new(AtomicU64::new(0));
    let indexer = Arc::new(TransferCounter {
        start,
        count: count.clone(),
    });

    // A short poll interval keeps the follow loop from delaying the test; the
    // immediate shutdown stops the engine right after the startup backfill.
    EventEngine::new(provider, db.clone())
        .register(indexer)
        .with_poll_interval(std::time::Duration::from_secs(3600))
        .run(async {})
        .await
        .expect("engine run");

    let indexed = count.load(Ordering::Relaxed);
    eprintln!("indexed {indexed} BZZ Transfer logs");
    assert!(indexed > 0, "expected a non-zero number of Transfer logs");

    let cursor = Cursor::load(db.as_ref(), "bzz_transfers")
        .expect("load cursor")
        .expect("cursor persisted");
    assert_eq!(cursor.last_block, head.number, "cursor advanced to head");
}
