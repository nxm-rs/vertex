//! End-to-end cross-checks against a real Gnosis Chain fork.
//!
//! One anvil-fork scaffold: it syncs the unified [`ContractIndexer`] over a
//! bounded recent window against a real provider, then cross-checks each view's
//! answer against an `eth_call` to the live contract at the same block. The
//! cross-checks are the only proof that (a) the indexer is wired to the RIGHT
//! contract addresses (a wrong address yields an empty view that does NOT match
//! the live contract's non-zero state) and (b) the verbatim-store decode-on-read
//! path reconstructs the same state the contract reports.
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

// This is an ignored, fork-only integration test; the cross-check helpers
// `.expect()` on the provider calls so the test body reads as a sequence of
// assertions. The whole file is test-only and never compiled into the library.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{Address, TxKind, U256};
use alloy_provider::network::TransactionBuilder;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_eth::{BlockId, TransactionRequest};
use alloy_sol_types::{SolCall, sol};
use vertex_chain_index::{ChainReader, Cursor, EventEngine};
use vertex_chain_index_contracts::{
    ContractId, ContractIndexer, INDEXER_NAME, Network, WatchedContract, registry, views,
};
use vertex_storage_redb::RedbDatabase;

/// How many blocks back from the finalized head to sync. The PostageStamp price
/// updates roughly once per redistribution round (152 blocks on Gnosis), so this
/// default spans several rounds and reliably captures a `PriceUpdate` while
/// keeping the `eth_getLogs` paging to a handful of pages. Overridable with
/// `CHAIN_INDEX_CONTRACTS_E2E_WINDOW`.
const DEFAULT_WINDOW: u64 = 6_000;

fn window() -> u64 {
    std::env::var("CHAIN_INDEX_CONTRACTS_E2E_WINDOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_WINDOW)
}

sol! {
    /// The PostageStamp pricing view functions the cross-check reads.
    #[allow(missing_docs)]
    function currentTotalOutPayment() external view returns (uint256);
    #[allow(missing_docs)]
    function lastPrice() external view returns (uint64);
    #[allow(missing_docs)]
    function lastUpdatedBlock() external view returns (uint64);
    /// The swap price oracle view (price, chequeValueDeduction).
    #[allow(missing_docs)]
    function getPrice() external view returns (uint256, uint256);
}

/// The watched address for `id` on mainnet, the cross-check's `eth_call` target.
fn address_of(id: ContractId) -> Address {
    registry(Network::Mainnet)
        .into_iter()
        .find(|c| c.id == id)
        .expect("contract in registry")
        .address
}

/// Call a unit-input view `C` at `block` via `eth_call` and decode its return.
async fn call_view<P: Provider, C: SolCall>(
    provider: &P,
    to: Address,
    call: C,
    block: u64,
) -> Result<C::Return, Box<dyn std::error::Error>> {
    let tx = TransactionRequest::default()
        .with_kind(TxKind::Call(to))
        .with_input(call.abi_encode());
    let returned = provider.call(tx).block(BlockId::number(block)).await?;
    Ok(C::abi_decode_returns(&returned)?)
}

/// Sync the unified indexer over a bounded recent window, then cross-check the
/// postage and swap views against `eth_call`s to the live contracts.
///
/// This is the regression guard for the contract-address wiring: if the registry
/// pointed at a wrong address (as nectar's book does for postage), the views
/// would be empty while the live contract reports a non-zero price, and the
/// cross-check below would fail.
#[tokio::test]
#[ignore = "requires a Gnosis fork or a permissive RPC; see module docs"]
async fn fork_sync_and_cross_check_views() {
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
    let start = head.number.saturating_sub(window());
    eprintln!(
        "syncing unified contract index over blocks {start}..={} via {rpc}",
        head.number
    );

    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let indexer = Arc::new(
        ContractIndexer::with_contracts(db.clone(), recent_window_registry(start))
            .expect("indexer"),
    );

    EventEngine::new(provider.clone(), db.clone())
        .register(indexer)
        .with_poll_interval(Duration::from_secs(3600))
        .run(async {})
        .await
        .expect("engine run");

    let cursor = Cursor::load(db.as_ref(), INDEXER_NAME)
        .expect("cursor read")
        .expect("the unified indexer must commit a cursor after a sync");
    assert_eq!(cursor.last_block, head.number, "cursor advanced to head");

    cross_check_postage(provider.as_ref(), db.as_ref(), head.number, start).await;
    cross_check_swap(provider.as_ref(), db.as_ref(), head.number).await;
}

/// Cross-check the postage pricing fold against the live PostageStamp.
///
/// Reconstructed `currentTotalOutPayment(head)`, `last_price`, and
/// `last_updated_block` must equal the contract's `currentTotalOutPayment()`,
/// `lastPrice()`, and `lastUpdatedBlock()` at the head, when the price's last
/// change falls inside the synced window. A non-zero on-chain price proves the
/// indexer is wired to the right contract.
async fn cross_check_postage<P: Provider>(provider: &P, db: &RedbDatabase, head: u64, start: u64) {
    let postage = address_of(ContractId::Postage);
    let projected = views::postage::chain_state(db).expect("read chain-state");
    eprintln!("projected postage chain-state: {projected:?}");

    let on_chain_total = call_view(provider, postage, currentTotalOutPaymentCall {}, head)
        .await
        .expect("currentTotalOutPayment");
    let on_chain_last_price = U256::from(
        call_view(provider, postage, lastPriceCall {}, head)
            .await
            .expect("lastPrice"),
    );
    let on_chain_last_updated = call_view(provider, postage, lastUpdatedBlockCall {}, head)
        .await
        .expect("lastUpdatedBlock");
    eprintln!(
        "on-chain currentTotalOutPayment({head})={on_chain_total}, \
         lastPrice={on_chain_last_price}, lastUpdatedBlock={on_chain_last_updated}"
    );
    assert!(
        on_chain_last_price > U256::ZERO,
        "the live PostageStamp reports a non-zero price: indexer is wired to the right contract"
    );

    match projected {
        Some(state) if on_chain_last_updated >= start => {
            assert_eq!(
                state.last_price, on_chain_last_price,
                "projected last_price matches the on-chain lastPrice()"
            );
            assert_eq!(
                state.last_updated_block, on_chain_last_updated,
                "projected last_updated_block matches the on-chain lastUpdatedBlock()"
            );
            assert_eq!(
                state.current_total_out_payment(head),
                on_chain_total,
                "reconstructed currentTotalOutPayment(head) matches the contract view"
            );
        }
        Some(state) => eprintln!(
            "lastUpdatedBlock {on_chain_last_updated} predates window start {start}; \
             skipping strict accumulator cross-check (projected={state:?})"
        ),
        None => assert!(
            on_chain_last_updated < start,
            "no PriceUpdate indexed, yet the last price change is inside the window: \
             the indexer should have folded it (wrong address?)"
        ),
    }
}

/// Cross-check the swap exchange-rate fold against the live oracle's
/// `getPrice()`. A wrong oracle address would leave the view empty while
/// `getPrice()` returns a non-zero price.
async fn cross_check_swap<P: Provider>(provider: &P, db: &RedbDatabase, head: u64) {
    let swap = address_of(ContractId::SwapPriceOracle);
    let price = call_view(provider, swap, getPriceCall {}, head)
        .await
        .expect("getPrice");
    let (on_chain_price, on_chain_deduction) = (price._0, price._1);
    eprintln!("on-chain getPrice() = (price {on_chain_price}, deduction {on_chain_deduction})");

    match views::swap::exchange_rate(db).expect("exchange_rate") {
        Some(rate) => assert_eq!(
            rate, on_chain_price,
            "projected exchange rate matches the on-chain getPrice().price"
        ),
        None => eprintln!(
            "no swap PriceUpdate in window; getPrice() still returns the constructor value \
             {on_chain_price} (grow the window to exercise the cross-check)"
        ),
    }
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
