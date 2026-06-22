//! Acceptance test for the wasm-clean client core.
//!
//! Runs on `wasm32-unknown-unknown` under `wasm-bindgen-test`: it composes the
//! accounting half of the shared client core (the pseudosettle provider embedded
//! through [`PseudosettleWiring::prepare`]) and round-trips a chunk through the
//! in-memory client cache, proving the settlement wiring and cache the launcher
//! assembles are reachable in the browser cone. The full [`assemble_client_core`]
//! also needs a `TopologyHandle`, which only a built node produces, so this
//! exercises the network-free half; the native launcher smoke test covers the
//! assembled whole.

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
