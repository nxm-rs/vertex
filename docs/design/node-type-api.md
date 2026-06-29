# Node-type API: the Client-centred builder

> Status note: the accounting and settlement half of this RFC predates the accounting and settlement reshape (epic #441), which removed the `BandwidthMode` runtime enum. The settlement model is now `feature = "swap"` plus `SwarmNodeType::swap_default()` plus an `Option<bool>` operator override, documented in `docs/design/accounting-settlement.md`. This RFC has been refreshed to that vocabulary: where it says "swap defaults on for storers" it means `swap_default()`, and "the swap override" means the `Option<bool>` (`--swap`) flag. The structural builder and seam design (the cache, reserve, settlement, pricer, and chain seams, and the three-level transition builder) is unchanged by the reshape. Read `docs/design/accounting-settlement.md` for the authoritative settlement model.

This note records the builder API for the three Swarm node types and the seams that make each component swappable. The target layering already exists structurally in the tree; the work is to promote internal closures to public methods, derive the swap default from the node type, wire the missing pseudosettle provider, keep the swap signal coherent, and establish the chain-provider and cache seams so a wasm node can run up to a Client-with-SWAP.

The maintainer has locked the four decisions that were previously open (storer chain is mandatory, the accounting `A` generic must flow, a storer gains a forwarding cache layered over its reserve, and swap selection is one coherent signal). They are recorded here as settled design, not as questions.

## 0. Summary

The progression light Client -> Client+SWAP CDN -> Storer is already real: `NodeBuilder -> ClientNodeBuilder` (`crates/swarm/builder/src/node.rs`) plus `StorerNodeBuilder`, which now lives behind the gated `reserve` seam module (`crates/swarm/builder/src/storer.rs`) and wraps a client builder internally, one shared assembly `build_client_backed_node` (`crates/swarm/builder/src/launch.rs`), and "a Storer IS a Client plus a reserve" is literally true. The only difference between the Client and Storer launch paths is the `StoreFactory` closure and one `node.enable_storage(reserve)` call (`launch.rs:341`). The four seams the maintainer wants pluggable (cache, reserve, settlement, pricer) are already object-safe traits.

The chain-provider seam is also already present in a different shape than first proposed: the chain is a shared `alloy_provider::Provider` (see `docs/design/chain-service.md`), not a parallel trait hierarchy, and alloy runs on `wasm32-unknown-unknown` with the right transport. So "chain access for a wasm SWAP client" is a transport-and-feature question, not a missing abstraction.

Two prerequisites gate everything and land first, in order: split the pseudosettle provider from the libp2p node crate, and converge the wasm `ClientLauncher` and native `build_client_backed_node` onto one wasm-clean client core. Without the first the light/wasm default cannot wire pseudosettle without poisoning the wasm cone. Without the second the wasm Client still has no accounting, cache, or forwarding, and the new builder seams are unreachable from the only path the wasm client uses.

## 1. Current-state API audit

### What is already a clean seam

- Cache. `SwarmLocalStore` (`crates/swarm/api/src/components/localstore.rs:33`), `auto_impl(&, Arc, Box)`, four methods over `CachedChunk`. Injected at the node level via `ClientNode::with_store(Arc<dyn SwarmLocalStore>)` (`crates/swarm/node/src/node/client.rs:413`). Default `ChunkStore` (`crates/swarm/localstore`). Wasm-clean.
- Reserve. `ReserveStore: SwarmLocalStore` plus `BinCursorStore` / `SettableRadius` (`crates/swarm/api/src/components/reserve.rs`). Bolted in via `ClientNode::enable_storage(Arc<dyn ReserveStore>)` (`client.rs:216`) which installs `StorerCapability` (`crates/swarm/node/src/protocol/storer.rs`). The `is_responsible_for -> put -> Receipt::sign` chain and reserve-backed retrieval already work. The reserve's `SwarmLocalStore::get` reads the `Payload` table (`crates/swarm/storer/src/db_reserve/store.rs:302`).
- Settlement. `SwarmSettlementProvider` (`crates/swarm/api/src/components/bandwidth.rs:66`), object-safe async trait, `auto_impl(&, Arc, Box)`, added via `AccountingBuilder::with_settlement` (`crates/swarm/bandwidth/core/src/builder.rs`). Pseudosettle and swap are sibling impls; pseudosettle is always wired for client-backed nodes, swap is wired when swap is enabled.
- Pricer. Generic `AccountingBuilder<C, P>`; `FixedPricer` / `NoPricer`.
- Node-type swap default has a home. `SwarmNodeType::swap_default()` (`crates/swarm/primitives/src/lib.rs:250`) returns true for a storer and false otherwise, mirroring `ConnectionProfile::default_for`.
- Chain access. `SharedChainProvider` and `build_chain_provider` (`crates/swarm/builder/src/chain.rs:32,68`) wrap a shared alloy provider behind the builder `chain` feature. `SwarmNodeType::needs_chain(swap_enabled)` (`primitives/src/lib.rs:275`) already encodes "storer always, client only with swap, bootnode never".

### The specific gaps, with paths

- The light client default is not actually light: zero providers wired. `build_client_backed_node` calls the pricer and reporter and conditionally the swap provider, but never adds a `PseudosettleProvider`. The default client runs an empty provider list: pure balance tracking, no time-based forgiveness. This is a correctness bug.
- Node-type swap default is not wired into construction. There is no path that resolves the effective swap state from the node type, so the storer-defaults-swap-on target silently does not happen.
- Cache and reserve are constructed inline, not injected, at the public builder. The client cache is hardcoded to `ChunkStore::with_budget(DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_SOC_CACHE_TTL_NS)` (`launch.rs:480`), ignoring `SwarmLocalStoreConfig`; the reserve is hardcoded `DbReserve` (`build_storer_reserve`, `launch.rs:559`). The `StoreFactory` / `NodeStore{local, reserve}` plumbing (`launch.rs:216-228`) is the right internal shape but is private.
- Swap selection is split across three signals, and they should be reconciled to one resolution site. The `swap` cargo feature is the compile gate, `SwarmNodeType::swap_default()` is the per-node-type default, and `SwapConfig.enable: Option<bool>` (`crates/swarm/node/src/args/swap.rs`) is the operator override. The effective swap state is `params.swap.enable.unwrap_or(node_type.swap_default())`, resolved once and shared with the chain precondition.
- The typed `A` parameter does not reach the providers. `ClientNodeBuilder<I, N, A>` is generic over accounting config, but `define_launch_types!` (`launch.rs:184-192`) pins `Accounting = ClientAccounting<Arc<Accounting<DefaultBandwidthConfig, Arc<Identity>>>, FixedPricer<Arc<Spec>>>`. `A` is named at the builder but discarded at the launch types.
- Two divergent assembly paths. Native `build_client_backed_node` wires accounting, forwarding, cache, and verify; the wasm `ClientLauncher::launch` (`crates/swarm/node/src/node/launch.rs`) builds a bare `ClientNode` with none of these and returns a raw handle, not a chunk provider.
- Reserve is invisible to operators. `StorerComponents<T, C, S>` erases `S` to `Arc<dyn SwarmLocalStore>` (`launch.rs:502-506`), so it impls `HasStore` but not `HasReserve`; reserve gRPC services are a TODO at `crates/swarm/rpc/src/adapter.rs:120`.
- Pseudosettle provider taints the wasm cone. The provider type is wasm-pure, but the crate hard-imports `vertex_swarm_node::ClientCommand` (`crates/swarm/bandwidth/pseudosettle/src/lib.rs:30`) and re-exports `PseudosettleEvent` (`:36`). Importing `PseudosettleProvider` drags libp2p into wasm.
- Chain build is degrade-not-fail for everyone. `build_node_chain_provider` returns `Ok(None)` and only warns when a chain-needing node has no RPC URL or no canonical deployment. A swap-defaulted storer with no RPC URL builds, accepts pushsync, signs receipts, and silently never settles or stakes. The locked decision makes this a hard error for a storer (section 2.8).

## 2. Recommended model

### 2.1 Node-type construction progression

Keep the builder layered by node type, but do not hang the storer transition off the client builder. The node-type axis is expressed by which builder you hold and which seams are wired, not by phantom type-state on every field. A type-state builder is rejected: it explodes the signatures FFI and gRPC must name, and it fights the cfg-gating. The storer build path now lives behind the gated `reserve` seam module in `vertex-swarm-builder`: the storer is built through `StorerNodeBuilder` (constructed via `from_config` / `from_parts` / `build`, wrapping a client builder internally), and the reserve seams (`with_reserve` / `with_reserve_factory`) live on it. The fluent `.with_storage()` transition that used to hang off the client builder was dropped so the storer cfg stays concentrated in the seam module and off the shared client builder.

### 2.2 Settlement default matrix

The swap default lives in one place: `SwarmNodeType::swap_default()` (`primitives/src/lib.rs:250`), resolved against the operator override at one site as `swap_enabled = params.swap.enable.unwrap_or(node_type.swap_default())`. The override is the `Option<bool>` from `SwapConfig.enable` (None == derive from node type), never inferred from value-equality. There is no runtime mode enum: pseudosettle is always on for client-backed nodes, and swap is the additive layer gated by the resolved `swap_enabled`.

| Node type | Swap default | Providers wired | Served store | Reserve | Chain | Wasm |
|---|---|---|---|---|---|---|
| Bootnode | n/a (no accounting) | none | none | none | no | n/a (native) |
| Client (light) | off | `PseudosettleProvider` (fix) | in-memory `ChunkStore`, budget from cfg | none | no | yes, this IS the wasm cone |
| Client + SWAP (CDN) | off, `--swap` opts in | `Pseudosettle` + `Swap` | in-memory `ChunkStore` | none | yes (cheque) | yes, feature + RPC provider (section 4) |
| Storer | on (`swap_default()`) | `Pseudosettle` + `Swap` | cache-then-reserve composite (section 2.5) | `DbReserve` or injected | yes, mandatory (section 2.8) | no (native) |

Rules:

- The light Client default equals the wasm Client cone equals `bandwidth-core` (pseudosettle only, in-memory cache, no chain or swap).
- `PseudosettleProvider` is always wired for a client-backed node. This fixes the empty-provider-list bug. Pseudosettle stays active on a Storer because a Storer still relays to light clients that cannot issue cheques; the swap layer is added on top.
- A Storer runs both pseudosettle and swap: keep pseudosettle for light peers, run swap with redistribution peers.

### 2.3 Where the swap signal lives

The effective swap state is resolved at exactly one site: `swap_enabled = params.swap.enable.unwrap_or(node_type.swap_default())`. Three orthogonal questions, three answers:

- "Is swap on?" answered by the resolved `swap_enabled`: the node-type default (`swap_default()`) unless the operator override (`SwapConfig.enable: Option<bool>`, the `--swap` flag) says otherwise. There is no runtime mode enum.
- "Turn swap on for this client." one builder method, `ClientNodeBuilder::with_swap(SwapConfig)` (CDN). Calling it sets the override and supplies the chequebook. For a Storer swap is on by node-type default; `with_swap` still supplies the chequebook.
- "Can this build contain swap at all?" the `swap` cargo feature, unchanged in its compile-gating role. `with_swap`, the `swap` field, and `SwapConfig` are gated so a build without the feature cannot reference them.

The override stays an `Option<bool>` so `None` means "use the node-type default" and an explicit `Some(false)` can disable swap on a storer without value-equality guessing. `with_swap` sets the override to `Some(true)` and records the chequebook; that the override is honoured over the node-type default is the documented behaviour.

### 2.4 In-memory-default swappable cache

`ClientNodeBuilder::with_cache(Arc<dyn SwarmLocalStore>)` and a lazy `with_cache_factory(FnOnce(Option<Arc<RedbDatabase>>) -> ...)` for persistent caches. Default is in-memory `ChunkStore`, now sized from `SwarmLocalStoreConfig::cache_budget_bytes()` (today the config is ignored for clients). This promotes the private `StoreFactory` to the public surface.

The cache seam carries three backends through one trait object:

- In-memory `ChunkStore` (all targets, the default everywhere).
- redb-backed (native), via `with_cache_factory` receiving the opened shared `RedbDatabase`.
- IndexedDB-backed (wasm), for a browser node acting as a cache or forwarder. This is a future impl, but the seam accommodates it now: `SwarmLocalStore` is `Send + Sync` with synchronous methods, and the wasm IndexedDB backend is supplied as an `Arc<dyn SwarmLocalStore>` through `with_cache`, gated by `cfg(target_arch = "wasm32")` and a feature, exactly like the redb backend is gated for native. Naming IndexedDB here keeps the wasm persistent-cache slot reserved against the same trait the native backends use.

### 2.5 Storer served store: cache-then-reserve composite

The current tree already makes a storer's served `SwarmLocalStore` its reserve: `build_storer_reserve` returns `NodeStore { local, reserve }` as two trait-object views of one `Arc<DbReserve>` (`launch.rs:594-599`), and `construct::storer` wires `parts.store` (the `local` view) into the components (`launch.rs:539`). `reserve.get` reads the `Payload` table; the six reserve tables are `Payload`, `Entry`, `BatchGroup`, `Replay`, `BinCounter`, `StampIndex` (`store.rs:26`).

The locked decision adds a second concern: a storer should also serve chunks it forwards or retrieves that fall outside its area of responsibility, CDN-style. The reserve must not hold those; admission gates the reserve to in-AoR stamped chunks under the redistribution rules, and the six tables are an atomic refcounted set that is not collapsible to host an out-of-AoR cache. So a storer's served store becomes a layered composite:

- Read path (retrieval-serve view, one `Arc<dyn SwarmLocalStore>`): a `CacheThenReserve` composite answers `get` / `contains` if either the cache or the reserve has the chunk. Precedence on overlapping addresses is reserve-first for `get` (the in-AoR copy is the authoritative, admission-validated one), cache as fallback. This stays a single trait object for the components and the gRPC store view.
- Write path: routes by responsibility. In-AoR pushsync ingest goes to the reserve (the existing `enable_storage(reserve)` path, unchanged). Forwarded or retrieved out-of-AoR chunks go to the cache. The composite does not auto-route writes from one `put`; the two write sites are distinct in the node layer (pushsync ingest vs. the forwarder and retrieval-serve cache-on-read), so each names its target store directly.
- Pushsync-ingest view stays separate. The reserve is still injected as its own `Arc<dyn ReserveStore>` so `is_responsible_for -> reserve.put -> Receipt::sign` is unchanged. Only the retrieval-serve view is the composite.

This makes `with_cache` meaningful on a Storer: it supplies the forwarding cache layer. The default storer forwarding cache is the in-memory `ChunkStore` (same default as a client), and a persistent forwarding cache is supplied through `with_cache_factory` (redb native, IndexedDB wasm where applicable, though a storer itself is native).

`CacheThenReserve` is a small composite in the builder or node layer holding `cache: Arc<dyn SwarmLocalStore>` and `reserve: Arc<dyn SwarmLocalStore>`, itself implementing `SwarmLocalStore`. It is the served view only; it never collapses the reserve tables, and it never lets a cache write reach the reserve admission path.

### 2.6 Reserve bolt-in API

Runtime mechanism unchanged (the cleanest seam in the tree): `enable_storage(Arc<dyn ReserveStore>)` installs `StorerCapability` into the single `ClientNodeBehaviour`; inbound pushsync does `is_responsible_for -> reserve.put -> Receipt::sign`; retrieval serves from the composite (section 2.5). Do not split a second `StorerNode` behaviour.

Public seam: `StorerNodeBuilder::with_reserve(Arc<dyn ReserveStore>)` and `with_reserve_factory(...)`, the public face of `build_storer_reserve`. Default stays the admission-gated `DbReserve`.

Single-injection: take one injected `Arc<dyn ReserveStore>` and derive the retrieval-serve view by arc-upcast at the call site, exactly as `launch.rs:597-598` already does (`Arc::clone(&reserve) as Arc<dyn SwarmLocalStore>` and `reserve as Arc<dyn ReserveStore>`). The arc-upcast works at MSRV 1.92. Do not add `ReserveStore::into_local_store`; an `Arc<Self>` receiver is incompatible with `auto_impl(&, Arc, Box)`. With the composite of section 2.5, the upcast reserve view becomes the `reserve` leg of `CacheThenReserve`.

Ordering safety (`enable_forwarding` and `enable_storage` run before the event loop) is enforced structurally: `build()` calls them in order; the operator never calls them post-build.

gRPC reach: carry `S = Arc<dyn ReserveStore>` (or `Arc<dyn BinCursorStore>` per `HasReserve::Reserve`) in `StorerComponents`, keeping `S` a type parameter so the api crate stays wasm-clean, impl `HasReserve`, and add a native-only `ReserveService` (closes `adapter.rs:120`). Land the type change and the service together.

### 2.7 Forwarding

Forwarding is node-type-uniform: every client-backed node calls `enable_forwarding`; `NetworkForwarder` earns the spread via two-leg deferred-credit accounting. The only role difference is whether inbound pushsync terminates in the reserve (Storer) or relays (Client / CDN), and, for a storer, whether forwarded chunks land in the forwarding cache (section 2.5). Do not promote the `pub(crate)` `Forwarder` trait to a public seam here: it is libp2p-coupled in the node crate and cannot become an api-crate trait object without dragging libp2p toward the wasm cone. File it as a separate post-v1 issue.

### 2.8 Storer chain is mandatory

Locked decision: a Storer uses dual settlement (pseudosettle and swap, with swap on by `swap_default()`), and a Storer build hard-fails if the chain does not resolve. A storer needs the chain for postage regardless of settlement (it stakes, reads the price oracle, and indexes batches), and once it holds a provider for postage it holds it for the chequebook, so chain is mandatory for a storer. The `Ok(None)` degrade path must not apply to a storer.

Concretely, `build_node_chain_provider` keeps `Ok(None)` only for a pure light client (which has no chain at all). For a storer (and for a swap-enabled CDN client, which has elected swap and therefore needs the chequebook on chain), a missing RPC URL or a network with no canonical deployment is a build-time error, not a warning. Use one coherent error variant, for example `SwarmNodeError::ChainRequired { node_type }`, raised inside `build_node_chain_provider` when `node_type.needs_chain(swap_enabled)` holds and the provider could not be constructed. This changes startup semantics, so it is a scoped change with its own test, not folded into the seam-promotion work.

### 2.9 Accounting pluggability: make `A` flow

Locked decision: make the `A` accounting config generic actually flow through to the providers, rather than pinning it in the macro and pretending. `SwarmClientAccounting` has associated types `Bandwidth` / `Pricing` and is `auto_impl(&, Arc)` only (`bandwidth.rs:227-233`), so it is not dyn-compatible without naming the concretes, and `PeerSelector` / `NetworkForwarder` consume `accounting.bandwidth()` / `pricing()` by concrete associated type on per-message hot paths. So "flow `A`" means keeping it a real generic end to end, not erasing to a trait object.

Blast radius, stated honestly:

- `define_launch_types!` (`launch.rs:184-192`) pins the accounting bundle today. The `with_client` arm must become generic over the accounting config and pricer, so `SwarmClientTypes::Accounting` is built from `A` rather than from the fixed `DefaultBandwidthConfig` / `FixedPricer<Arc<Spec>>`.
- `ClientComponents` and `StorerComponents` name the concrete accounting in their bounds; threading `A` means carrying the accounting type parameter (or its associated bandwidth and pricing types) through the component structs and the gRPC adapter.
- The FFI and gRPC configs name the concrete `*Config` today; with `A` flowing, the default surfaces still pin the default bundle (FFI and gRPC do not need to be generic over `A`), but the builder type machinery must not discard it. The escape hatch for a genuinely different accounting struct is the node-level `ClientNode` + `AccountingBuilder<C, P>` layer, which is already fully generic.

Treat this as a real workstream in the migration plan (section 6, step 7), separable from the seam-promotion work, because it touches the launch types, the component generics, and every site that names `ClientAccounting<...>`.

## 3. Concrete builder and constructor API

```rust
// ============================================================================
// crates/swarm/primitives/src/lib.rs  -- already present (lib.rs:250).
// Swap defaults on for a Storer (maximum support) and off otherwise.
// ============================================================================
impl SwarmNodeType {
    pub fn swap_default(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }
}

// ============================================================================
// crates/swarm/builder/src/launch.rs  -- one resolution site, shared with the
// chain precondition. The override is the Option<bool> from SwapConfig.enable.
// ============================================================================
#[cfg(feature = "swap")]
let swap_enabled = params.swap.enable.unwrap_or(node_type.swap_default());
#[cfg(not(feature = "swap"))]
let swap_enabled = false;

// ============================================================================
// crates/swarm/builder/src/node.rs  -- ClientNodeBuilder gains injectable seams.
// All new fields are Option/Vec; None == wasm-clean default. `A` flows to providers.
// ============================================================================
pub struct ClientNodeBuilder<I, N, A>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
{
    base: NodeBuilder<I, N>,
    accounting: A,
    verify: ChunkVerifyConfig,
    cache: Option<CacheSeam>,
    extra_settlements: Vec<Box<dyn SwarmSettlementProvider>>,
    #[cfg(feature = "chain")] chain: ChainConfig,
    #[cfg(feature = "swap")]  swap: Option<SwapConfig>, // Some(_) == swap ON
}

/// Cache supplied eagerly, or lazily so the opened shared db can flow in.
/// The trait object accepts in-memory (all targets), redb (native), and
/// IndexedDB (wasm) backends without a generic explosion.
pub enum CacheSeam {
    Ready(Arc<dyn SwarmLocalStore>),
    Factory(Box<dyn FnOnce(Option<Arc<RedbDatabase>>)
        -> Result<Arc<dyn SwarmLocalStore>, SwarmNodeError> + Send>),
}

impl<I, N, A> ClientNodeBuilder<I, N, A> /* same bounds */ {
    pub fn with_verify(self, verify: ChunkVerifyConfig) -> Self;

    /// Replace the default in-memory cache (default budget now read from
    /// SwarmLocalStoreConfig). Wasm-safe trait object, no redb in the signature.
    pub fn with_cache(mut self, cache: Arc<dyn SwarmLocalStore>) -> Self;
    pub fn with_cache_factory<F>(mut self, f: F) -> Self
    where F: FnOnce(Option<Arc<RedbDatabase>>)
        -> Result<Arc<dyn SwarmLocalStore>, SwarmNodeError> + Send + 'static;

    /// Bleeding-edge settlement experiment, appended after the node-type base set.
    pub fn with_settlement(mut self, p: impl SwarmSettlementProvider + 'static) -> Self;

    /// THE swap seam. Calling it == swap ON for a CDN client (no reserve):
    /// records the chequebook, sets the swap override to enabled, and marks the
    /// chain needed. Feature-gated, not target-gated, so a wasm SWAP client can
    /// call it given a wasm chain-provider transport (section 4).
    #[cfg(feature = "swap")]
    pub fn with_swap(mut self, swap: SwapConfig) -> Self;

    #[cfg(feature = "chain")]
    pub fn with_chain(self, chain: ChainConfig) -> Self;

    // No storer transition here: the client builder carries no storer-only
    // seams. A storer is built through StorerNodeBuilder (below), gated by the
    // `reserve` feature, so the storer cfg stays off the shared client builder.

    pub async fn build(self, ctx: &dyn InfrastructureContext)
        -> Result<BuiltClient, SwarmNodeError>;
}

// ============================================================================
// StorerNodeBuilder -- lives behind the `reserve` seam module; reserve is
// injectable, default stays DbReserve. Constructed via from_config / from_parts
// (it wraps a client builder internally), never via a client-builder transition.
// The forwarding cache is the inherited `with_cache` seam (section 2.5).
// ============================================================================
#[cfg(feature = "reserve")]
impl<I, N, A, S, St> StorerNodeBuilder<I, N, A, S, St> /* existing bounds */ {
    /// Construct from a storer config (yields the concrete DefaultStorerBuilder).
    pub fn from_config(config: StorerConfig) -> DefaultStorerBuilder;
    /// Construct from already-built parts (yields DefaultStorerBuilder).
    pub fn from_parts(/* spec, identity, network, accounting, store, storage, verify */)
        -> DefaultStorerBuilder;

    /// Inject any reserve. One Arc yields both the pushsync-ingest ReserveStore
    /// view and the `reserve` leg of the CacheThenReserve served view
    /// (arc upcast, as launch.rs:597-598 already does today).
    pub fn with_reserve(mut self, reserve: Arc<dyn ReserveStore>) -> Self;
    pub fn with_reserve_factory<F>(mut self, f: F) -> Self
    where F: FnOnce(Option<Arc<RedbDatabase>>, &Arc<Identity>, u64 /*capacity*/)
        -> Result<Arc<dyn ReserveStore>, SwarmNodeError> + Send + 'static;

    pub async fn build(self, ctx: &dyn InfrastructureContext)
        -> Result<BuiltStorer, SwarmNodeError>;
}
```

### Happy paths

```rust
// 1. Light client (wasm-clean default): pseudosettle wired, in-memory cache, no chain.
//    swap_default() is false for a Client, so swap stays off without an override.
let client = NodeBuilder::new(spec, identity, network)
    .with_accounting(accounting_cfg)
    .build(ctx).await?;

// 2. CDN client: swap on via the override, NO reserve, forwarding cache is the client cache.
let cdn = NodeBuilder::new(spec, identity, network)
    .with_accounting(accounting_cfg)
    .with_swap(SwapConfig { chequebook, beneficiary: None, deploy: false }) // override -> swap on
    .build(ctx).await?;

// 3. Storer: swap defaulted ON by swap_default(), default DbReserve, chain mandatory.
//    Built through StorerNodeBuilder (the `reserve` seam), not a client-builder transition.
//    from_config wires chain and swap from the StorerConfig; build hard-fails without chain.
let storer = StorerNodeBuilder::from_config(storer_cfg)
    .build(ctx).await?;

// 3b. Storer with a custom reserve and a persistent forwarding cache:
let storer = StorerNodeBuilder::from_config(storer_cfg)
    .with_cache_factory(|db| Ok(Arc::new(RedbForwardCache::open(db)?) as _)) // forwarding layer
    .with_reserve(Arc::new(MyReserve::new()) as Arc<dyn ReserveStore>)
    .build(ctx).await?;
```

### Swappable traits and default impls

| Seam | Trait (crate) | Default impl | Native-only? |
|---|---|---|---|
| Cache | `SwarmLocalStore` (api) | `ChunkStore` (localstore); IndexedDB (wasm, future); redb (native, future) | no, wasm default |
| Reserve | `ReserveStore` (+ `BinCursorStore` / `SettableRadius`) (api) | `DbReserve` (storer) | yes |
| Settlement | `SwarmSettlementProvider` (api) | `PseudosettleProvider`; `SwapProvider` | provider: no, swap: feature-gated |
| Pricer | `SwarmPricing` via `AccountingBuilder<C, P>` | `FixedPricer` | no |
| Chain | shared `alloy_provider::Provider` (`SharedChainProvider`, builder) | wallet-filled native HTTP provider; wasm fetch/WS provider (future) | no, feature + transport |

## 4. FFI / gRPC / wasm projection

All three drive the same `ClientConfig` / `StorerConfig` -> `Default*Builder` path; the projection is about which seams are present per surface, enforced by cfg and feature, not by divergent assemblies.

- gRPC (desktop and server ops). Native-only by design (tonic + libp2p + tokio); never in the wasm cone. The CLI keeps its `match node_type` constructing the matching `*Config`. The per-container `RegistersGrpcServices` dispatch stays finite because the seams are trait objects. Add the native-only `ReserveService` gated on `HasReserve` once `StorerComponents` carries the typed reserve.
- FFI (native and mobile embedding). `VertexClientConfig` gains optional swap fields mapping to `with_swap`, gated under the ffi `chain` feature (which forwards to the builder chain feature). `with_cache_factory` / `with_reserve_factory` and any `RedbDatabase`-typed signature stay `cfg(not(target_arch = "wasm32"))` in the ffi crate too, mirroring the builder.
- wasm (browser). Two configurations now live in the wasm cone, an additive split, not one fixed shape:
  - Light client. `db_path = None` (in-memory cache), pseudosettle only, no swap, no reserve. This stays the minimal wasm cone and needs no feature beyond the base; it is just "never call the native-only methods."
  - Client + SWAP. Additive: the `swap` feature plus a wasm-compatible chain provider. Swap and chain are not target-gated off on wasm. The chain code already compiles for wasm given the right transport (alloy `Provider` with `default-features = false`, no reqwest or native TLS; see `docs/design/chain-service.md`). A wasm SWAP client supplies an RPC endpoint reachable from the browser (fetch or WS JSON-RPC) and a wasm provider impl built over it. The persistent cache, where used, is the IndexedDB backend (section 2.4).

### cfg discipline that keeps the wasm cone clean

The old "gate the whole swap surface by `cfg(not(target_arch = "wasm32"))`" rule is replaced by gating per feature and per provider impl:

- The swap and chain code (`with_swap`, `SwapConfig`, the settlement service, the chequebook contract reads) is gated by the `swap` and `chain` cargo features, not by `target_arch`. It compiles for wasm when those features are on and a wasm chain-provider transport is supplied.
- What stays genuinely native-only and `cfg(not(target_arch = "wasm32"))`: anything pulling tokio-only runtime pieces, libp2p, redb, or tonic. The storer reserve surface (`StorerNodeBuilder` with `with_reserve` / `with_reserve_factory`, and `RedbDatabase`-typed signatures) is concentrated behind the `reserve` feature seam module and stays out of the wasm and default-client cones (a storer is native). The native HTTP chain provider construction stays native; the wasm provider is a sibling impl behind `cfg(target_arch = "wasm32")`.
- The provider impl, not the swap surface, is what differs by target: native uses a direct alloy HTTP/WS provider; wasm uses an alloy provider over a browser-reachable RPC. Both satisfy the same `alloy_provider::Provider` bound the settlement service consumes.

Extend `just check-cone` to assert (a) the split pseudosettle provider stays in the wasm cone and the node crate does not, (b) the reserve and redb surface is absent from the wasm and FFI cones, and (c) the swap and chain surface compiles for `wasm32-unknown-unknown` under the `swap` and `chain` features with the wasm provider impl. The libp2p boundary and the FFI/gRPC-only public-API rule are unchanged: wasm swap exposure is through wasm-bindgen and the same library API, never HTTP+JSON.

### Node-type matrix across surfaces

| Configuration | gRPC | FFI | wasm |
|---|---|---|---|
| Bootnode | yes | n/a | no |
| Client (light) | yes | yes | yes (base cone) |
| Client + SWAP (CDN) | yes | yes (chain feature) | yes (swap + chain features + wasm RPC provider) |
| Storer | yes | yes (chain feature) | no (native: redb, libp2p pullsync, mandatory chain) |

## 5. Swappability story (no builder fork)

A bleeding-edge implementer replaces a component by passing a trait object; no edit to `launch.rs`, no fork of the builder:

- Custom cache: implement `SwarmLocalStore`, pass `Arc::new(MyCache) as Arc<dyn SwarmLocalStore>` to `with_cache`, or a `with_cache_factory` closure for a db-backed cache. On a storer this supplies the forwarding cache layer.
- Custom reserve: implement `ReserveStore` (+ `BinCursorStore` / `SettableRadius`), pass to `StorerNodeBuilder::with_reserve`. The one Arc serves both ingest and the reserve leg of the served composite via arc upcast.
- Custom settlement scheme: implement `SwarmSettlementProvider`, pass to `with_settlement`; it is appended after the node-type base providers (pseudosettle, and swap when enabled).
- Custom chain transport: supply a wasm or native alloy provider over your own RPC; the settlement service consumes the `Provider` bound, not a concrete client.
- Genuinely different accounting struct (rare): drop to the node-level `ClientNode` builder + `AccountingBuilder<C, P>`, which is fully generic. With `A` flowing (section 2.9) the typed `ClientNodeBuilder` already carries the config and pricer; the node-level layer is the escape hatch for a different bundle.

## 6. Migration plan (each step independently shippable)

1. Split the pseudosettle provider from the node crate (hard prerequisite). Extract a wasm-clean `PseudosettleProvider` with zero `vertex_swarm_node` deps (remove the `ClientCommand` import at `pseudosettle/src/lib.rs:30`; leave `PseudosettleService` / `PseudosettleHandle` / `PseudosettleEvent` in the native service path or behind a cfg gate). Extend `just check-cone`. No builder change yet.
2. Wire pseudosettle as the default provider for every client-backed node. Always add `PseudosettleProvider` in `build_client_backed_node`; it has no per-node-type gate. Validate pseudosettle refresh semantics against the reference (a live-path behavioural change, not a wire change).
3. Resolve swap selection at one site. Derive the effective swap state as `swap_enabled = swap.enable.unwrap_or(node_type.swap_default())` in `build_client_backed_node` and share it with the chain precondition. The `--swap` flag is the `Option<bool>` override (None defers to `swap_default()`); the `swap` cargo feature stays the compile gate.
4. Promote the cache and reserve seams to public builder methods. `with_cache` / `with_cache_factory`, `with_reserve` / `with_reserve_factory`; derive both reserve views by the arc-upcast already in `launch.rs:597-598`; route `cache_budget_bytes` through to the client default cache. `with_settlement` passthrough.
5. Storer cache-then-reserve composite. Add `CacheThenReserve` as the storer retrieval-serve view, route forwarded/out-of-AoR writes to the cache and in-AoR pushsync to the reserve, keep the reserve view separate for ingest. `with_cache` becomes the forwarding-cache seam on a storer.
6. Make the swap seam idiomatic. `ClientNodeBuilder::with_swap(SwapConfig)` records the chequebook, sets the swap override to enabled, and marks chain needed. Feature-gated, not target-gated.
7. Make `A` flow. Generalise `define_launch_types!`'s `with_client` arm over the accounting config and pricer, thread the accounting type parameter (or its associated bandwidth and pricing types) through `ClientComponents` / `StorerComponents` and the gRPC adapter, and stop discarding `A` at the launch types. Separable workstream with its own review.
8. `StorerComponents` `HasReserve` + `ReserveService`. Carry typed `S` so `HasReserve` is impl-able; add the native-only gRPC reserve service (closes `adapter.rs:120`). Land the type change and service together.
9. Storer chain hard-fail. Make `build_node_chain_provider` return a build error (`SwarmNodeError::ChainRequired`) for a storer or a swap-enabled CDN client whose chain did not resolve; keep `Ok(None)` only for a pure light client. Own test, scoped PR (changes startup semantics).
10. wasm ClientCore convergence (first-class workstream, own design note). One wasm-clean core (accounting with pseudosettle, in-memory `ChunkStore`, `NetworkForwarder`) shared by `ClientLauncher` and `build_client_backed_node`; swap/chain the additive feature-gated add-ons with the wasm chain provider. Add a `wasm32-unknown-unknown` build test asserting the launched client has a populated provider list and a working cache, and a second asserting a swap-enabled wasm client compiles with the wasm provider impl.
11. wasm SWAP and IndexedDB cache impls (later). The wasm alloy provider over a browser RPC and the IndexedDB `SwarmLocalStore` backend. The seams from steps 4 and 6 must already accommodate them; these steps fill the slots.

## 7. Open questions

The four previously-open items (storer chain mandatory, `A` flows, storer forwarding cache, `--swap` deleted) are decided and live in the design body above. The remaining open questions come from the wasm-SWAP-via-RPC and storer-layered-store designs.

- Chequebook reads from a browser over an untrusted RPC. A wasm SWAP client reads chequebook balance and cashout state through whatever RPC endpoint the browser is given. An untrusted or malicious RPC can lie about chain state. What is the trust model: does the wasm client accept the operator-supplied RPC as trusted (the same posture as a native node trusting its `--chain.rpc-url`), or does it need light-client verification or a second source before acting on a cheque? For v1 of the wasm SWAP client, trusting the supplied RPC matches the native posture, but it should be stated as a known limitation.
- Cache and reserve precedence on overlapping addresses. Section 2.5 sets reserve-first for `get` on the storer served composite. Confirm there is no case where a forwarding-cache copy should win (for example a fresher SOC version landing in the cache before the reserve admits it). SOC freshness on a storer is governed by the reserve's stamp-timestamp ordering; the forwarding cache holds out-of-AoR chunks it is not responsible for, so the overlap should be empty in practice, but the composite must not silently serve a stale cache copy of an in-AoR address.
- Eviction interaction between cache and reserve. The forwarding cache is byte-bounded LRU; the reserve is capacity-bounded with furthest-from-neighbourhood eviction. They are separate stores with separate budgets, so they do not evict each other, but a storer's total resident footprint is now cache budget plus reserve capacity. Confirm the operator-facing sizing story (two independent budgets) and whether the forwarding cache budget should default lower on a storer than on a pure client.
- wasm SWAP runtime audit. Beyond the chain provider, confirm the swap settlement service path holds no tokio-only timer or task that lacks a wasm equivalent through `vertex-tasks` / `web-time`. The settlement service spawns work; that spawn must resolve to the wasm executor in the wasm build, or the swap surface is feature-gated-but-not-actually-wasm-runnable.

## Relevant files

`crates/swarm/builder/src/node.rs`, `crates/swarm/builder/src/launch.rs` (`StoreFactory` / `NodeStore` at :216-228, `define_launch_types!` at :184-192, client `make_store` at :478-486, `build_storer_reserve` at :559-599, dual-Arc upcast at :597-598, `build_node_chain_provider` at :123, chain wiring at :370-383), `crates/swarm/builder/src/chain.rs:32,68` (`SharedChainProvider`, `build_chain_provider`), `crates/swarm/builder/src/swap.rs`, `crates/swarm/node/src/args/swap.rs` (`--swap` flag and `SwapConfig.enable: Option<bool>`), `crates/swarm/bandwidth/core/src/config.rs`, `crates/swarm/bandwidth/core/src/builder.rs`, `crates/swarm/bandwidth/pseudosettle/src/lib.rs:30,36` (`ClientCommand` import, `PseudosettleEvent` re-export), `crates/swarm/primitives/src/lib.rs:275` (`needs_chain`), `:250` (`swap_default`), `crates/swarm/api/src/components/localstore.rs:33` (`SwarmLocalStore`), `crates/swarm/api/src/components/reserve.rs`, `crates/swarm/api/src/components/bandwidth.rs:66,227`, `crates/swarm/storer/src/db_reserve/store.rs:26,302` (six tables, `Payload`-table `get`), `crates/swarm/node/src/node/client.rs:216,413` (`enable_storage`, `with_store`), `crates/swarm/node/src/node/launch.rs` (`ClientLauncher`), `crates/swarm/rpc/src/adapter.rs:120`, `crates/ffi/src/api/client.rs`, `bin/vertex/src/cli.rs`, `docs/design/chain-service.md`.
