//! End-to-end smoke test against a real Gnosis Chain fork.
//!
//! It runs the real [`StakingIndexer`] over a bounded recent window of finalized
//! blocks served by an anvil fork (or a live RPC), then cross-checks one indexed
//! owner's projection row against a direct `stakes(owner)` contract call. This
//! validates the fold against real on-chain `StakeRegistry` logs rather than the
//! synthetic ones the unit tests use.
//!
//! It is `#[ignore]`d so CI without a fork stays green. To run it:
//!
//! 1. Start a fork (anvil ships with foundry; install via `foundryup` if absent).
//!    The public Gnosis RPC is shared and rate-limited, so fork at a recent block
//!    and let the test scan only a bounded window below the finalized head:
//!
//!    ```sh
//!    anvil --fork-url ${GNOSIS_RPC_URL:-https://rpc.gnosischain.com} \
//!          --fork-block-number <recent> --port 8549 --silent &
//!    ```
//!
//!    then point the test at it (this is also the default):
//!
//!    ```sh
//!    STAKING_E2E_RPC=http://localhost:8549 \
//!        cargo test -p vertex-chain-index-staking --test e2e_fork -- --ignored --nocapture
//!    ```
//!
//! 2. If anvil cannot be installed or run, point the test directly at a public
//!    Gnosis RPC over the same bounded window:
//!
//!    ```sh
//!    STAKING_E2E_RPC=https://rpc.gnosischain.com \
//!        cargo test -p vertex-chain-index-staking --test e2e_fork -- --ignored --nocapture
//!    ```
//!
//! The window size is bounded by `WINDOW` below the RPC's current finalized
//! block, so the test stays inside the public RPC's `eth_getLogs` limits and
//! finishes quickly on a fork and on a live RPC alike.

use std::sync::Arc;

use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_eth::TransactionRequest;
use alloy_sol_types::SolCall;
use nectar_contracts::IStakeRegistry;
use vertex_chain_index::{ChainReader, Cursor, EventEngine};
use vertex_chain_index_staking::{STAKE_REGISTRY, StakeProjection, StakingIndexer};
use vertex_storage_redb::RedbDatabase;

/// How many blocks back from the finalized head to scan. Bounded so a single
/// `eth_getLogs` sweep stays inside the public RPC's range and result limits.
const WINDOW: u64 = 40_000;

#[tokio::test]
#[ignore = "requires a Gnosis fork or live RPC; see module docs"]
async fn indexes_real_stake_registry_logs() {
    let rpc =
        std::env::var("STAKING_E2E_RPC").unwrap_or_else(|_| "http://localhost:8549".to_string());

    let url = rpc.parse().expect("STAKING_E2E_RPC is a valid URL");
    let provider = Arc::new(ProviderBuilder::new().connect_http(url));

    // Bound the window to the RPC's finalized head. Skip (do not fail) if the RPC
    // is unreachable so a missing fork leaves the suite green.
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
        "syncing StakeRegistry logs over blocks {start}..={} via {rpc}",
        head.number
    );

    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    // Start at the bounded window, not the deployment block, so the test scans a
    // small recent range rather than the contract's full history.
    let indexer = Arc::new(
        StakingIndexer::new(db.clone())
            .expect("init projection")
            .with_start_block(start),
    );

    // A long poll interval plus an immediate shutdown stops the engine right
    // after the startup backfill completes.
    EventEngine::new(provider.clone(), db.clone())
        .register(indexer)
        .with_poll_interval(std::time::Duration::from_secs(3600))
        .run(async {})
        .await
        .expect("engine run");

    let cursor = Cursor::load(db.as_ref(), "staking_registry")
        .expect("load cursor")
        .expect("cursor persisted");
    assert_eq!(cursor.last_block, head.number, "cursor advanced to head");

    let proj = StakeProjection::new(db.as_ref());
    let overlays = proj.staked_overlays().expect("read staked overlays");
    eprintln!("indexed {} staked overlays in the window", overlays.len());

    // The window may legitimately contain no StakeUpdated events; if it does,
    // cross-check one indexed owner against a direct contract call. This proves
    // the fold reflects on-chain state, not just that decoding succeeded.
    let Some((overlay, owner)) = overlays.first().copied() else {
        eprintln!("no staking activity in the window; skipping the contract cross-check");
        return;
    };

    let row = proj
        .stake_of(owner)
        .expect("read owner row")
        .expect("indexed owner has a row");
    assert_eq!(row.overlay, overlay, "row overlay matches the set entry");

    // Call `stakes(owner)` directly: encode the call, `eth_call` it, decode the
    // return. The shared `IStakeRegistry` `sol!` interface generates the call and
    // return types; no instance wrapper is needed for a single read.
    let calldata = IStakeRegistry::stakesCall { owner }.abi_encode();
    let request = TransactionRequest::default()
        .to(STAKE_REGISTRY)
        .input(calldata.into());
    let raw = provider.call(request).await.expect("stakes() eth_call");
    let on_chain =
        IStakeRegistry::stakesCall::abi_decode_returns(&raw).expect("decode stakes() return");

    eprintln!(
        "owner {owner}: indexed potential={} committed={} overlay={overlay}; \
         on-chain potential={} committed={} overlay={}",
        row.potential,
        row.committed,
        on_chain.potentialStake,
        on_chain.committedStake,
        on_chain.overlay
    );

    // The contract's current `stakes(owner)` reflects the latest StakeUpdated for
    // that owner. If our window captured that owner's most recent update, the
    // projection must agree with the live view.
    assert_eq!(
        row.overlay, on_chain.overlay,
        "indexed overlay matches the on-chain stakes() overlay"
    );
}
