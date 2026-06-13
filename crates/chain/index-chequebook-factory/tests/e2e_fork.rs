//! End-to-end smoke test against a real Gnosis Chain fork (or live RPC).
//!
//! This validates the chequebook-factory indexer against real on-chain state: it
//! syncs the [`ChequebookFactoryIndexer`] over a bounded recent window, asserts
//! the engine reaches the finalized head and the cursor advances, and
//! cross-checks one recorded chequebook against a direct
//! `deployedContracts(address)` call on the factory (which the factory returns
//! `true` for exactly the addresses it deployed).
//!
//! # On the window
//!
//! The Gnosis mainnet SimpleSwapFactory
//! (`0xc2d5a532cf69aa9a1378737d8ccdef884b6e7420`) deploys a chequebook whenever a
//! new node funds one, so a recent window normally contains some
//! `SimpleSwapDeployed` logs. It can, however, be quiet: if the window held no
//! deployment the test still asserts the engine reached head over real data and
//! skips the membership cross-check. Set
//! `CHEQUEBOOK_FACTORY_E2E_FROM_DEPLOYMENT=1` to sync the whole contract lifetime
//! instead of a recent window; on a fork that proxies `eth_getLogs` upstream this
//! is slower but guarantees deployments to cross-check.
//!
//! It is `#[ignore]`d so CI without a fork stays green. To run it:
//!
//! 1. Start a fork (anvil ships with foundry; install via `foundryup` if absent).
//!    Use the UNIQUE port 8548 so it does not collide with sibling indexers:
//!
//!    ```sh
//!    anvil --fork-url ${GNOSIS_RPC_URL:-https://rpc.gnosischain.com} \
//!          --fork-block-number <recent> --port 8548 --silent &
//!    CHEQUEBOOK_FACTORY_E2E_RPC=http://localhost:8548 \
//!        cargo test -p vertex-chain-index-chequebook-factory --test e2e_fork \
//!        -- --ignored --nocapture
//!    ```
//!
//! 2. If anvil cannot be run, point the test at a public Gnosis RPC directly
//!    (shared and rate-limited, so keep the window bounded):
//!
//!    ```sh
//!    CHEQUEBOOK_FACTORY_E2E_RPC=https://rpc.gnosischain.com \
//!        cargo test -p vertex-chain-index-chequebook-factory --test e2e_fork \
//!        -- --ignored --nocapture
//!    ```

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::TxKind;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_eth::{BlockId, TransactionInput, TransactionRequest};
use alloy_sol_types::{SolCall, SolValue};
use nectar_contracts::{ChequebookFactory, IChequebookFactory, mainnet};
use vertex_chain_index::{ChainReader, Cursor, EventEngine};
use vertex_chain_index_chequebook_factory::{
    ChequebookFactoryIndexer, ChequebookFactoryTable, ChequebookKey,
};
use vertex_storage::{Database, DbTx};
use vertex_storage_redb::RedbDatabase;

/// How many blocks back from the finalized head to sync by default. A modest
/// window keeps a fork-proxied backfill fast; set
/// `CHEQUEBOOK_FACTORY_E2E_FROM_DEPLOYMENT=1` to sync the whole contract
/// lifetime.
const WINDOW: u64 = 500_000;

#[tokio::test]
#[ignore = "requires a Gnosis fork or live RPC; see module docs"]
async fn indexes_real_chequebook_factory() {
    let rpc = std::env::var("CHEQUEBOOK_FACTORY_E2E_RPC")
        .unwrap_or_else(|_| "http://localhost:8548".to_string());

    let url = rpc
        .parse()
        .expect("CHEQUEBOOK_FACTORY_E2E_RPC is a valid URL");
    let provider = Arc::new(ProviderBuilder::new().connect_http(url));

    // Bound the window to the RPC's finalized head. Skip (do not fail) if the RPC
    // is unreachable so the test is a smoke check, not a hard dependency.
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

    let deployment = mainnet::CHEQUEBOOK_FACTORY;
    let from_deployment = std::env::var("CHEQUEBOOK_FACTORY_E2E_FROM_DEPLOYMENT").is_ok();
    let start = if from_deployment {
        deployment.block
    } else {
        head.number.saturating_sub(WINDOW).max(deployment.block)
    };
    let factory = ChequebookFactory::new(deployment.address, start);
    eprintln!(
        "syncing chequebook factory {} over blocks {start}..={} via {rpc}",
        deployment.address, head.number
    );

    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let indexer = Arc::new(ChequebookFactoryIndexer::new(factory, db.clone()));
    indexer.init().expect("init projection table");

    // A long poll interval keeps the follow loop from delaying the test; the
    // immediate shutdown stops the engine right after the startup backfill.
    EventEngine::new(provider.clone(), db.clone())
        .register(indexer)
        .with_poll_interval(Duration::from_secs(3600))
        .run(async {})
        .await
        .expect("engine run");

    // The engine fully backfilled to the finalized head: the cursor advanced.
    let cursor = Cursor::load(db.as_ref(), "chequebook_factory")
        .expect("load cursor")
        .expect("cursor persisted");
    assert_eq!(cursor.last_block, head.number, "cursor advanced to head");

    let deployed: Vec<ChequebookKey> = db
        .view(|tx| {
            Ok(tx
                .entries::<ChequebookFactoryTable>()?
                .into_iter()
                .map(|(k, _)| k)
                .collect())
        })
        .expect("read deployed set");
    eprintln!("indexed {} factory-deployed chequebooks", deployed.len());

    // Cross-check against real chain data: the factory's `deployedContracts`
    // mapping returns `true` for exactly the addresses it deployed. Take one
    // chequebook we recorded and assert the factory agrees it deployed it; this
    // proves the indexer folded a real `SimpleSwapDeployed` payload off the chain.
    match deployed.first() {
        Some(ChequebookKey(chequebook)) => {
            let call = IChequebookFactory::deployedContractsCall { addr: *chequebook };
            let tx = TransactionRequest {
                to: Some(TxKind::Call(deployment.address)),
                input: TransactionInput::new(call.abi_encode().into()),
                ..Default::default()
            };
            let returned = provider
                .call(tx)
                .block(BlockId::number(head.number))
                .await
                .expect("deployedContracts eth_call");
            let on_chain = bool::abi_decode(&returned).expect("decode deployedContracts return");
            eprintln!("factory.deployedContracts({chequebook}) = {on_chain}");
            assert!(
                on_chain,
                "the factory confirms it deployed a chequebook the indexer recorded",
            );
        }
        None => {
            eprintln!(
                "no SimpleSwapDeployed in window; engine reached head over real data \
                 (set CHEQUEBOOK_FACTORY_E2E_FROM_DEPLOYMENT=1 to force deployments)"
            );
        }
    }
}
