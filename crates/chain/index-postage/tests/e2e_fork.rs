//! End-to-end smoke test against a real Gnosis Chain fork (or live RPC).
//!
//! This validates the postage indexer against real on-chain logs rather than
//! synthetic ones: it syncs the [`PostageIndexer`] over a bounded recent window,
//! asserts the engine reaches the finalized head and the cursor advances, then
//! cross-checks the reconstructed pricing chain-state against direct
//! `currentTotalOutPayment()`, `lastPrice()`, and `lastUpdatedBlock()` contract
//! reads at the finalized head.
//!
//! # The cross-check
//!
//! The indexer reconstructs the contract's `totalOutPayment` accumulator from the
//! `PriceUpdate` cadence alone. The strong proof is that the projection's
//! `current_total_out_payment(head)` equals the contract's
//! `currentTotalOutPayment()` view at the same block. That only holds if the
//! indexer saw *every* `PriceUpdate` since the accumulator's current anchor, so
//! the cross-check is gated on the synced window reaching back past the
//! contract's `lastUpdatedBlock()`:
//!
//! - If `lastUpdatedBlock()` falls inside the synced window, the projection's
//!   `last_price` / `last_updated_block` must equal the on-chain values and the
//!   computed `current_total_out_payment(head)` must equal
//!   `currentTotalOutPayment()`.
//! - If the last price change predates the window (no `PriceUpdate` in range),
//!   the projection's chain-state stays empty while the contract still reports a
//!   non-zero price, proving the indexer is wired to the right live contract and
//!   the engine reached head over real data. Grow `POSTAGE_E2E_WINDOW` to capture
//!   a price change and exercise the full accumulator cross-check.
//!
//! It is `#[ignore]`d so CI without a fork stays green. To run it:
//!
//! 1. Start a fork (anvil ships with foundry; install via `foundryup` if absent).
//!    Use a UNIQUE port (8546) so it does not collide with sibling indexers:
//!
//!    ```sh
//!    anvil --fork-url ${GNOSIS_RPC_URL:-https://rpc.gnosischain.com} \
//!          --fork-block-number <recent> --port 8546 --silent &
//!    POSTAGE_E2E_RPC=http://localhost:8546 \
//!        cargo test -p vertex-chain-index-postage --test e2e_fork \
//!        -- --ignored --nocapture
//!    ```
//!
//! 2. If anvil cannot be run, point the test at a public Gnosis RPC directly
//!    (this is also the default URL):
//!
//!    ```sh
//!    POSTAGE_E2E_RPC=https://rpc.gnosischain.com \
//!        cargo test -p vertex-chain-index-postage --test e2e_fork \
//!        -- --ignored --nocapture
//!    ```
//!
//! The window size and head are derived from the RPC's current finalized block,
//! so the test works against both a fork and a live RPC. The public RPC is shared
//! and rate-limited; the default window is bounded to a handful of `eth_getLogs`
//! pages and the engine uses a generous poll interval that runs exactly one
//! startup backfill.

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{TxKind, U256};
use alloy_provider::network::TransactionBuilder;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_eth::{BlockId, TransactionRequest};
use alloy_sol_types::{SolCall, sol};
use vertex_chain_index::{ChainReader, Cursor, EventEngine};
use vertex_chain_index_postage::{
    INDEXER_NAME, POSTAGE_STAMP_ADDRESS, PostageIndexer, read_chain_state,
};
use vertex_storage_redb::RedbDatabase;

sol! {
    /// The PostageStamp view functions the cross-check reads.
    #[allow(missing_docs)]
    function currentTotalOutPayment() external view returns (uint256);
    #[allow(missing_docs)]
    function lastPrice() external view returns (uint64);
    #[allow(missing_docs)]
    function lastUpdatedBlock() external view returns (uint64);
}

/// How many blocks back from the finalized head to sync, overridable with
/// `POSTAGE_E2E_WINDOW`.
///
/// The price oracle updates the PostageStamp price roughly once per
/// redistribution round (152 blocks on Gnosis), so this default spans several
/// rounds and reliably captures a `PriceUpdate` while keeping the `eth_getLogs`
/// paging to a handful of pages.
const DEFAULT_WINDOW: u64 = 6_000;

fn window() -> u64 {
    std::env::var("POSTAGE_E2E_WINDOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_WINDOW)
}

/// Call a unit-input view `C` at `block` via `eth_call` and decode its return.
///
/// Returns the error rather than panicking so the lone `#[tokio::test]` owns the
/// assertions; the test `.expect()`s each call where the `allow-expect-in-tests`
/// clippy config applies.
async fn call_view<P: Provider, C: SolCall>(
    provider: &P,
    call: C,
    block: u64,
) -> Result<C::Return, Box<dyn std::error::Error>> {
    let tx = TransactionRequest::default()
        .with_kind(TxKind::Call(POSTAGE_STAMP_ADDRESS))
        .with_input(call.abi_encode());
    let returned = provider.call(tx).block(BlockId::number(block)).await?;
    Ok(C::abi_decode_returns(&returned)?)
}

#[tokio::test]
#[ignore = "requires a Gnosis fork or live RPC; see module docs"]
async fn indexes_real_postage_logs() {
    let rpc = std::env::var("POSTAGE_E2E_RPC")
        .unwrap_or_else(|_| "https://rpc.gnosischain.com".to_string());

    let url = rpc.parse().expect("POSTAGE_E2E_RPC is a valid URL");
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
        "syncing PostageStamp logs over blocks {start}..={} via {rpc}",
        head.number
    );

    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let indexer =
        Arc::new(PostageIndexer::with_start_block(db.clone(), start).expect("build indexer"));

    // A long poll interval plus an immediate shutdown runs exactly one startup
    // backfill of the bounded window, then stops.
    EventEngine::new(provider.clone(), db.clone())
        .register(indexer)
        .with_poll_interval(Duration::from_secs(3600))
        .run(async {})
        .await
        .expect("engine run");

    // The engine fully backfilled to the finalized head: the cursor advanced.
    let cursor = Cursor::load(db.as_ref(), INDEXER_NAME)
        .expect("load cursor")
        .expect("cursor persisted");
    assert_eq!(cursor.last_block, head.number, "cursor advanced to head");

    let projected = read_chain_state(db.as_ref()).expect("read chain-state");
    eprintln!("projected chain-state: {projected:?}");

    // Read the on-chain pricing state at the head.
    let on_chain_total = call_view(
        provider.as_ref(),
        currentTotalOutPaymentCall {},
        head.number,
    )
    .await
    .expect("currentTotalOutPayment");
    let on_chain_last_price = U256::from(
        call_view(provider.as_ref(), lastPriceCall {}, head.number)
            .await
            .expect("lastPrice"),
    );
    let on_chain_last_updated = call_view(provider.as_ref(), lastUpdatedBlockCall {}, head.number)
        .await
        .expect("lastUpdatedBlock");
    eprintln!(
        "on-chain currentTotalOutPayment({})={on_chain_total}, lastPrice={on_chain_last_price}, lastUpdatedBlock={on_chain_last_updated}",
        head.number
    );
    assert!(
        on_chain_last_price > U256::ZERO,
        "the live PostageStamp reports a non-zero price: indexer is wired to the right contract"
    );

    // The cross-check holds only when the synced window reaches back past the
    // accumulator's current anchor (so the indexer saw the PriceUpdate that set
    // it). Otherwise the projection is legitimately empty.
    match projected {
        Some(state) if on_chain_last_updated >= start => {
            assert_eq!(
                state.last_price, on_chain_last_price,
                "projected last_price matches the on-chain lastPrice()",
            );
            assert_eq!(
                state.last_updated_block, on_chain_last_updated,
                "projected last_updated_block matches the on-chain lastUpdatedBlock()",
            );
            assert_eq!(
                state.current_total_out_payment(head.number),
                on_chain_total,
                "reconstructed currentTotalOutPayment(head) matches the contract view",
            );
        }
        Some(state) => {
            eprintln!(
                "lastUpdatedBlock {on_chain_last_updated} predates window start {start}; \
                 skipping strict accumulator cross-check (projected={state:?})"
            );
        }
        None => {
            assert!(
                on_chain_last_updated < start,
                "no PriceUpdate indexed, yet the last price change is inside the window: \
                 the indexer should have folded it",
            );
            eprintln!(
                "no PriceUpdate in window; projection empty while contract reports a price \
                 (grow POSTAGE_E2E_WINDOW to exercise the accumulator cross-check)"
            );
        }
    }
}
