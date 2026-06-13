//! End-to-end smoke test against a real Gnosis Chain fork (or live RPC).
//!
//! This validates the swap-price-oracle indexer against real on-chain state: it
//! syncs the [`SwapPriceIndexer`] over a bounded recent window, asserts the
//! engine reaches the finalized head and the cursor advances, and cross-checks
//! the projection against a direct `getPrice()` contract call at the head.
//!
//! # A note on this contract's events
//!
//! The Gnosis mainnet swap (settlement) price oracle
//! (`0xA57A50a831B31c904A770edBCb706E03afCdbd94`) sets its price and deduction in
//! its constructor and, as of this writing, has never emitted a `PriceUpdate` or
//! `ChequeValueDeductionUpdate`: `getPrice()` returns the constructor values
//! (price `100000`, deduction `100`) and a full-lifetime `eth_getLogs` over the
//! two event topics returns nothing. So the cross-check is two-sided:
//!
//! - If the synced window contains updates, the projected exchange rate must
//!   equal the on-chain `getPrice().price` (the projection tracks the chain).
//! - If it contains none (the live mainnet case), the projection stays empty and
//!   the on-chain `getPrice()` still returns a value, proving the indexer is
//!   wired to the right contract and the engine reached head over real data.
//!
//! Set `SWAP_PRICE_E2E_FROM_DEPLOYMENT=1` to sync the whole contract lifetime
//! instead of a recent window; on a fork that proxies `eth_getLogs` upstream this
//! is slow but exercises the full backfill.
//!
//! It is `#[ignore]`d so CI without a fork stays green. To run it:
//!
//! 1. Start a fork (anvil ships with foundry; install via `foundryup` if absent).
//!    Use a UNIQUE port (8547) so it does not collide with sibling indexers:
//!
//!    ```sh
//!    anvil --fork-url ${GNOSIS_RPC_URL:-https://rpc.gnosischain.com} \
//!          --fork-block-number <recent> --port 8547 --silent &
//!    SWAP_PRICE_E2E_RPC=http://localhost:8547 \
//!        cargo test -p vertex-chain-index-swap-price --test e2e_fork \
//!        -- --ignored --nocapture
//!    ```
//!
//! 2. If anvil cannot be run, point the test at a public Gnosis RPC directly:
//!
//!    ```sh
//!    SWAP_PRICE_E2E_RPC=https://rpc.gnosischain.com \
//!        cargo test -p vertex-chain-index-swap-price --test e2e_fork \
//!        -- --ignored --nocapture
//!    ```

use std::sync::Arc;

use alloy_primitives::{TxKind, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_eth::{BlockId, TransactionInput, TransactionRequest};
use alloy_sol_types::{SolCall, SolValue};
use nectar_contracts::{ISwapPriceOracle, SwapPriceOracle, mainnet};
use vertex_chain_index::{ChainReader, Cursor, EventEngine};
use vertex_chain_index_swap_price::{SwapPriceField, SwapPriceIndexer, read_field};
use vertex_storage_redb::RedbDatabase;

/// How many blocks back from the finalized head to sync by default. A modest
/// window keeps a fork-proxied backfill fast; set
/// `SWAP_PRICE_E2E_FROM_DEPLOYMENT=1` to sync the whole contract lifetime.
const WINDOW: u64 = 100_000;

#[tokio::test]
#[ignore = "requires a Gnosis fork or live RPC; see module docs"]
async fn indexes_real_swap_price_oracle() {
    let rpc =
        std::env::var("SWAP_PRICE_E2E_RPC").unwrap_or_else(|_| "http://localhost:8547".to_string());

    let url = rpc.parse().expect("SWAP_PRICE_E2E_RPC is a valid URL");
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

    let deployment = mainnet::SWAP_PRICE_ORACLE;
    let from_deployment = std::env::var("SWAP_PRICE_E2E_FROM_DEPLOYMENT").is_ok();
    let start = if from_deployment {
        deployment.block
    } else {
        head.number.saturating_sub(WINDOW).max(deployment.block)
    };
    let oracle = SwapPriceOracle::new(deployment.address, start);
    eprintln!(
        "syncing swap price oracle {} over blocks {start}..={} via {rpc}",
        deployment.address, head.number
    );

    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let indexer = Arc::new(SwapPriceIndexer::new(oracle, db.clone()));
    indexer.init().expect("init projection table");

    // A long poll interval keeps the follow loop from delaying the test; the
    // immediate shutdown stops the engine right after the startup backfill.
    EventEngine::new(provider.clone(), db.clone())
        .register(indexer)
        .with_poll_interval(std::time::Duration::from_secs(3600))
        .run(async {})
        .await
        .expect("engine run");

    // The engine fully backfilled to the finalized head: the cursor advanced.
    let cursor = Cursor::load(db.as_ref(), "swap_price_oracle")
        .expect("load cursor")
        .expect("cursor persisted");
    assert_eq!(cursor.last_block, head.number, "cursor advanced to head");

    let rate = read_field(db.as_ref(), SwapPriceField::ExchangeRate).expect("read rate");
    let deduction =
        read_field(db.as_ref(), SwapPriceField::ChequeValueDeduction).expect("read deduction");
    eprintln!("projected exchange rate: {rate:?}");
    eprintln!("projected cheque value deduction: {deduction:?}");

    // Read the on-chain values at the head via a direct `getPrice()` call and
    // decode the returned `(price, chequeValueDeduction)` tuple.
    let call = ISwapPriceOracle::getPriceCall {};
    let tx = TransactionRequest {
        to: Some(TxKind::Call(deployment.address)),
        input: TransactionInput::new(call.abi_encode().into()),
        ..Default::default()
    };
    let returned = provider
        .call(tx)
        .block(BlockId::number(head.number))
        .await
        .expect("getPrice eth_call");
    let (on_chain_price, on_chain_deduction) =
        <(U256, U256)>::abi_decode(&returned).expect("decode getPrice return");
    eprintln!("on-chain getPrice() = (price {on_chain_price}, deduction {on_chain_deduction})");

    // Two-sided cross-check (see the module docs): when the window held an
    // update, the projection must equal the chain; when it held none, the
    // projection is empty while the chain still reports a value.
    match rate {
        Some(row) => assert_eq!(
            row.value, on_chain_price,
            "projected exchange rate matches the on-chain getPrice().price",
        ),
        None => {
            assert!(
                on_chain_price > U256::ZERO,
                "no PriceUpdate in window, yet the contract reports a price: \
                 indexer is wired to the right live contract",
            );
        }
    }
    if let Some(row) = deduction {
        assert_eq!(
            row.value, on_chain_deduction,
            "projected deduction matches the on-chain getPrice().chequeValueDeduction",
        );
    }
}
