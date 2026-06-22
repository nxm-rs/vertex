//! Acceptance test for the wasm-clean client core.
//!
//! Runs on `wasm32-unknown-unknown` under `wasm-bindgen-test`: it composes the
//! accounting half of the shared client core (the pseudosettle provider embedded
//! through [`PseudosettleWiring::prepare`], plus SWAP under the `swap` feature)
//! and round-trips a chunk through the client cache, proving the settlement
//! wiring and cache the launcher assembles are reachable in the browser cone.
//! Under the `indexeddb` feature it also round-trips a chunk through the
//! IndexedDB-backed cache. The full [`assemble_client_core`] also needs a
//! `TopologyHandle`, which only a built node produces, so this exercises the
//! network-free half; the native launcher smoke test covers the assembled whole.

#![cfg(target_arch = "wasm32")]
#![allow(clippy::expect_used)]

use nectar_primitives::{AnyChunk, ContentChunk};
use vertex_swarm_accounting::{AccountingBuilder, DefaultBandwidthConfig};
use vertex_swarm_api::{SwarmClientAccounting, SwarmIdentity, SwarmLocalStore};
use vertex_swarm_localstore::{ChunkStore, DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_SOC_CACHE_TTL_NS};
use vertex_swarm_node::PseudosettleWiring;
use vertex_swarm_primitives::CachedChunk;
use vertex_swarm_test_utils::test_identity_arc;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn client_core_accounting_wires_pseudosettle_on_wasm() {
    let identity = test_identity_arc();
    let bandwidth = DefaultBandwidthConfig::default();

    // The launcher prepares the pseudosettle provider before the accounting is
    // built; the core then embeds it through the same builder tail.
    let (provider, _wiring) = PseudosettleWiring::prepare(&bandwidth);
    let accounting = AccountingBuilder::new(bandwidth)
        .with_pricer_from_config(identity.spec().clone())
        .with_settlement(provider)
        .build(&identity);

    let names = accounting.bandwidth().provider_names();
    assert!(
        names.contains(&"pseudosettle"),
        "expected pseudosettle in the provider list, got {names:?}"
    );
}

#[wasm_bindgen_test]
fn client_cache_round_trips_on_wasm() {
    let store = ChunkStore::with_budget(
        DEFAULT_CACHE_BUDGET_BYTES as usize,
        DEFAULT_SOC_CACHE_TTL_NS,
    );

    // A content chunk is immutable and stampless on the retrieval path, so the
    // cache serves it by address with no freshness signal.
    let chunk: AnyChunk = ContentChunk::new(&b"wasm client core round trip"[..])
        .expect("valid content chunk")
        .into();
    let cached = CachedChunk::new(chunk, None);
    let address = *cached.address();

    store.put(cached.clone()).expect("put");
    assert!(store.contains(&address), "cache must contain the chunk");
    assert_eq!(store.get(&address).expect("get"), Some(cached));
}

/// A swap-enabled client registers both settlement providers, pseudosettle first
/// (soft accounting) and swap second (originated-debt settlement), composed
/// exactly as the launcher's swap tail does. A chain-free config is enough: the
/// provider list does not depend on cashout.
#[cfg(feature = "swap")]
#[wasm_bindgen_test]
fn client_core_accounting_wires_pseudosettle_and_swap_on_wasm() {
    use alloy_primitives::Address;
    use vertex_swarm_api::SwarmSettlementProvider;
    use vertex_swarm_node::SwapWiring;

    let identity = test_identity_arc();
    let bandwidth = DefaultBandwidthConfig::default();
    let spec = identity.spec().clone();

    let (pseudosettle_provider, _) = PseudosettleWiring::prepare(&bandwidth);
    let (swap_provider, _) = SwapWiring::prepare(
        &spec,
        &identity,
        &bandwidth,
        Some(Address::repeat_byte(0xab)),
        None,
        false,
        0,
        true,
    )
    .expect("swap wiring is prepared for a chequebook on a named chain");

    let accounting = AccountingBuilder::new(bandwidth)
        .with_pricer_from_config(spec)
        .with_settlement(pseudosettle_provider)
        .with_settlements(vec![
            Box::new(swap_provider) as Box<dyn SwarmSettlementProvider>
        ])
        .build(&identity);

    assert_eq!(
        accounting.bandwidth().provider_names(),
        vec!["pseudosettle", "swap"],
        "a swap-enabled client reports pseudosettle then swap"
    );
}

/// The IndexedDB-backed cache round-trips a chunk, mirroring the in-memory case
/// over the persisted backend the browser client supplies through `with_store`.
/// Built (not run) in CI as the wasm-reachability gate; the round-trip executes
/// only under a headless browser runner.
#[cfg(feature = "indexeddb")]
#[wasm_bindgen_test]
async fn indexeddb_cache_round_trips_on_wasm() {
    use vertex_storage_indexeddb::IndexedDbDatabase;
    use vertex_swarm_localstore::{IndexedDbBackend, SystemClock};

    let db = IndexedDbDatabase::open("vertex-swarm-cache-test", &[IndexedDbBackend::store_name()])
        .await
        .expect("open IndexedDB");
    let backend = IndexedDbBackend::new(db.into_arc(), DEFAULT_CACHE_BUDGET_BYTES as usize);
    let store = ChunkStore::with_backend(backend, DEFAULT_SOC_CACHE_TTL_NS, SystemClock);

    let chunk: AnyChunk = ContentChunk::new(&b"wasm indexeddb round trip"[..])
        .expect("valid content chunk")
        .into();
    let cached = CachedChunk::new(chunk, None);
    let address = *cached.address();

    store.put(cached.clone()).expect("put");
    assert!(store.contains(&address), "cache must contain the chunk");
    assert_eq!(store.get(&address).expect("get"), Some(cached));
}
