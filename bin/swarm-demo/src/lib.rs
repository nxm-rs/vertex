//! Browser Swarm client demo: a wasm-bindgen app that starts a client node and
//! renders its Kademlia topology, exposing a small JS-facing surface.

mod client;
mod files_ui;
mod ui;
mod worker;

pub use client::SwarmClient;

use std::collections::VecDeque;
use std::sync::Arc;

use alloy_primitives::Address;
use tracing::{Level, info, warn};
use vertex_net_dnsaddr_doh::{DohClient, resolve_mainnet_wss_bootnodes};
use vertex_storage_indexeddb::IndexedDbDatabase;
use vertex_swarm_api::{SwarmIdentity, SwarmLocalStore};
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::{
    ChunkStore, DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_SOC_CACHE_TTL_NS, IndexedDbBackend, SystemClock,
};
use vertex_swarm_node::{ClientLauncher, LauncherSwapConfig, SwarmNodeType};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_spec::{init_mainnet, mainnet_wss_bootnodes};
use vertex_swarm_topology::{KademliaConfig, TopologyEvent, TopologyHandle};
use vertex_tasks::TaskManager;
use wasm_bindgen::prelude::*;

/// IndexedDB database name for the browser client cache.
const CACHE_DB_NAME: &str = "vertex-swarm-cache";

/// Maximum topology events buffered between UI drains.
const EVENT_BUFFER_CAP: usize = 256;

/// Maximum pending peer connect/disconnect triggers buffered between UI drains.
const PEER_EVENT_BUFFER_CAP: usize = 256;

/// Upper bound on `score` events appended per drain.
const SCORE_EVENTS_PER_DRAIN: usize = 32;

/// A peer-topology trigger awaiting enrichment at drain time.
enum PeerTrigger {
    /// Peer completed handshake.
    Connect {
        overlay: OverlayAddress,
        peer_id: libp2p::PeerId,
        node_type: SwarmNodeType,
    },
    /// Peer's connections all closed.
    Disconnect { overlay: OverlayAddress },
}

/// A running browser Swarm client, exposed to JS.
#[wasm_bindgen]
pub struct SwarmDemo {
    topology: TopologyHandle<Arc<Identity>>,
    events: std::rc::Rc<std::cell::RefCell<VecDeque<TopologyEvent>>>,
    // Parallel buffer of peer connect/disconnect triggers fed from the same
    // topology subscription as `events`, drained by `drain_peer_events` and
    // enriched against the peer manager at drain time.
    peer_events: std::rc::Rc<std::cell::RefCell<VecDeque<PeerTrigger>>>,
    overlay: String,
    // The node's libp2p peer id as a base58 string, captured at launch. Handed
    // to JS via `self()` and on each `connect` peer event.
    peer_id: String,
    // The browser file client (upload/download/manifest walk) over the launched
    // node. Cheap to clone (Arc/Rc-backed); the accessor hands out a shared
    // handle over the same session cache.
    client: SwarmClient,
    // The task manager owns the global executor the node tasks were spawned
    // through. Held for the session so the spawned tasks keep running.
    _task_manager: TaskManager,
}

#[wasm_bindgen]
impl SwarmDemo {
    /// The node's ephemeral overlay address as a hex string.
    #[wasm_bindgen(getter)]
    pub fn overlay(&self) -> String {
        self.overlay.clone()
    }

    /// The browser file client (shared handle over the same session cache).
    #[wasm_bindgen(getter)]
    pub fn client(&self) -> SwarmClient {
        self.client.clone()
    }

    /// Snapshot the current readiness state as a plain JS object.
    #[wasm_bindgen]
    pub fn readiness(&self) -> JsValue {
        let snap = self.topology.readiness();
        let obj = js_sys::Object::new();
        set_u32(&obj, "connectedPeers", snap.connected_peers as u32);
        set_u32(&obj, "connectedStorers", snap.connected_storers as u32);
        set_u32(&obj, "depth", u32::from(snap.depth.get()));
        set_u32(
            &obj,
            "neighborhoodConnected",
            snap.neighborhood_connected as u32,
        );
        set_u32(
            &obj,
            "saturationThreshold",
            snap.saturation_threshold as u32,
        );
        set_u32(&obj, "binsAtTarget", snap.bins_at_target as u32);
        set_str(&obj, "phase", snap.phase.into());
        set_bool(&obj, "isRoutable", snap.is_routable());
        set_bool(&obj, "isSaturated", snap.is_saturated());

        let bins = js_sys::Array::new();
        for bin in &snap.bins {
            let entry = js_sys::Object::new();
            set_u32(&entry, "bin", u32::from(bin.bin.get()));
            set_u32(&entry, "connected", bin.connected as u32);
            match bin.target {
                Some(t) => set_u32(&entry, "target", t as u32),
                None => set_null(&entry, "target"),
            }
            set_u32(&entry, "deficit", bin.deficit as u32);
            bins.push(&entry);
        }
        set_value(&obj, "bins", &bins);

        obj.into()
    }

    /// Drain the topology events seen since the last call as a JS array.
    #[wasm_bindgen(js_name = drainEvents)]
    pub fn drain_events(&self) -> js_sys::Array {
        let out = js_sys::Array::new();
        let mut buf = self.events.borrow_mut();
        for event in buf.drain(..) {
            let obj = js_sys::Object::new();
            let (kind, detail) = describe_event(&event);
            set_str(&obj, "kind", kind);
            set_str(&obj, "detail", &detail);
            out.push(&obj);
        }
        out
    }

    /// This node's identity (`overlay`, `peerId`) as a JS object for the globe feed.
    #[wasm_bindgen(js_name = self)]
    pub fn self_info(&self) -> JsValue {
        let obj = js_sys::Object::new();
        set_str(&obj, "overlay", &self.overlay);
        set_str(&obj, "peerId", &self.peer_id);
        obj.into()
    }

    /// Drain peer connect/disconnect triggers and emit periodic score refreshes
    /// as a JS array for the globe feed.
    #[wasm_bindgen(js_name = drainPeerEvents)]
    pub fn drain_peer_events(&self) -> js_sys::Array {
        let out = js_sys::Array::new();
        let pm = self.topology.peer_manager();

        // Connect/disconnect triggers captured from the topology stream.
        let triggers: Vec<PeerTrigger> = self.peer_events.borrow_mut().drain(..).collect();
        for trigger in triggers {
            match trigger {
                PeerTrigger::Connect {
                    overlay,
                    peer_id,
                    node_type,
                } => {
                    let obj = js_sys::Object::new();
                    set_str(&obj, "type", "connect");
                    set_str(&obj, "overlay", &overlay.to_string());
                    set_str(&obj, "peerId", &peer_id.to_string());
                    set_str(&obj, "nodeType", node_type.into());

                    // multiaddrs: format each Multiaddr to its string form.
                    let multiaddrs = js_sys::Array::new();
                    if let Some(peer) = pm.get_swarm_peer(&overlay) {
                        for ma in peer.multiaddrs() {
                            multiaddrs.push(&JsValue::from_str(&ma.to_string()));
                        }
                    }
                    set_value(&obj, "multiaddrs", &multiaddrs);

                    // po: proximity-order bin of the peer relative to us.
                    set_u32(&obj, "po", u32::from(pm.index().bin_for(&overlay).get()));

                    // score: nested { total } object.
                    let total = pm.get_peer_score(&overlay).unwrap_or(0.0);
                    set_value(&obj, "score", &score_object(total));

                    // connectedAt: unix seconds -> ms for the JS Date math.
                    match pm.connected_since(&overlay) {
                        Some(secs) => set_value(
                            &obj,
                            "connectedAt",
                            &JsValue::from_f64((secs as f64) * 1000.0),
                        ),
                        None => set_value(&obj, "connectedAt", &JsValue::from_f64(0.0)),
                    }

                    // direction: 'inbound' | 'outbound' | null.
                    match pm.connection_direction(&overlay) {
                        Some(dir) => {
                            let s: &'static str = dir.into();
                            set_str(&obj, "direction", s);
                        }
                        None => set_null(&obj, "direction"),
                    }

                    out.push(&obj);
                }
                PeerTrigger::Disconnect { overlay } => {
                    let obj = js_sys::Object::new();
                    set_str(&obj, "type", "disconnect");
                    set_str(&obj, "overlay", &overlay.to_string());
                    out.push(&obj);
                }
            }
        }

        // Periodic score refresh for currently connected peers (bounded). A peer
        // is "currently connected" iff it has a live connection timestamp.
        let connected: Vec<OverlayAddress> = pm
            .index()
            .all_peers()
            .into_iter()
            .filter(|overlay| pm.connected_since(overlay).is_some())
            .take(SCORE_EVENTS_PER_DRAIN)
            .collect();
        for overlay in connected {
            let obj = js_sys::Object::new();
            set_str(&obj, "type", "score");
            set_str(&obj, "overlay", &overlay.to_string());
            let total = pm.get_peer_score(&overlay).unwrap_or(0.0);
            set_value(&obj, "score", &score_object(total));
            out.push(&obj);
        }

        out
    }
}

/// Build a `{ total: <f64> }` JS object for a peer score.
fn score_object(total: f64) -> JsValue {
    let obj = js_sys::Object::new();
    set_value(&obj, "total", &JsValue::from_f64(total));
    obj.into()
}

/// wasm entrypoint: start the client and keep the handle alive for the session.
#[wasm_bindgen(start)]
pub fn main() {
    // The wasm start hook fires on every module instantiation, including inside
    // a Web Worker (which has no `window`/DOM). The UI boot is main-thread only;
    // in a worker the entry point is `startWorkerNode`, so skip the DOM path
    // here when there is no document.
    if web_sys::window().is_none() {
        return;
    }
    wasm_bindgen_futures::spawn_local(async {
        match start().await {
            Ok(demo) => {
                // Publish the handle on `window.__swarmDemo` for the globe
                // frontend's `?live` peer feed, and keep the client alive for
                // the page session through that JS-owned reference.
                publish_handle(demo);
            }
            Err(e) => {
                tracing::error!(?e, "failed to start Swarm client demo");
                ui::set_status("failed to start, see console");
            }
        }
    });
}

/// Start the browser Swarm client and mount the topology UI.
///
/// # Errors
/// Returns a JS error if the client node fails to build or start.
#[wasm_bindgen]
pub async fn start() -> Result<SwarmDemo, JsValue> {
    console_error_panic_hook::set_once();
    init_tracing();
    apply_retrieval_overrides_from_page();

    info!("starting browser Swarm client demo");

    // Establish the global executor before building the node: the topology
    // tasks, peer-manager tick, and client service all resolve their spawner
    // through `TaskExecutor::current`, which reads the executor this manager
    // installs. The manager is held in the returned handle for the session.
    let task_manager = TaskManager::current();

    let spec = init_mainnet();
    let identity = Identity::random(spec, SwarmNodeType::Client);
    let overlay = identity.overlay_address().to_string();

    ui::mount(&overlay);
    ui::set_status("resolving bootnodes...");

    let bootnodes =
        resolve_mainnet_wss_bootnodes(&DohClient::cloudflare(), mainnet_wss_bootnodes()).await;
    info!(count = bootnodes.len(), "resolved bootnodes");
    ui::set_status(&format!(
        "dialing {} bootnodes over wss...",
        bootnodes.len()
    ));

    // The IndexedDB-backed cache survives a page reload; a failed open falls
    // back to the launcher's in-memory default so the demo still runs.
    let mut launcher = ClientLauncher::new(identity).with_bootnodes(bootnodes);
    if let Some(kademlia) = kademlia_config_from_page() {
        launcher = launcher.with_kademlia(kademlia);
    }
    match open_indexeddb_store().await {
        Ok(store) => {
            info!("using IndexedDB-backed client cache");
            launcher = launcher.with_store(store);
        }
        Err(e) => warn!(?e, "IndexedDB cache unavailable, using the in-memory cache"),
    }

    // SWAP cheque settlement is enabled when a chequebook address is supplied
    // through the page config; without one the client settles by pseudosettle
    // alone. Cheque exchange is chain-free unless an RPC URL is also supplied.
    if let Some(swap) = swap_config_from_page() {
        info!(chequebook = %swap.chequebook, "SWAP settlement enabled");
        launcher = launcher.with_swap(swap);
    }

    let launched = launcher
        .launch()
        .await
        .map_err(|e| JsValue::from_str(&format!("failed to launch client: {e}")))?;
    let topology = launched.topology().clone();
    let peer_id = launched.local_peer_id().to_string();

    // Build the browser file client over the launched node; it captures the
    // client handle and topology it needs, so the `LaunchedClient` can be
    // dropped after this.
    let client = SwarmClient::from_launched(&launched);

    // Mount the upload/download/manifest UI below the topology view.
    files_ui::mount(client.clone());

    let events = std::rc::Rc::new(std::cell::RefCell::new(VecDeque::with_capacity(
        EVENT_BUFFER_CAP,
    )));
    let peer_events = std::rc::Rc::new(std::cell::RefCell::new(VecDeque::with_capacity(
        PEER_EVENT_BUFFER_CAP,
    )));
    spawn_event_pump(topology.clone(), events.clone(), peer_events.clone());
    spawn_render_loop(topology.clone());

    ui::set_status("connecting...");

    let demo = SwarmDemo {
        topology,
        events,
        peer_events,
        overlay,
        peer_id,
        client,
        _task_manager: task_manager,
    };
    Ok(demo)
}

/// Open the IndexedDB-backed client cache.
///
/// Hydrates the resident LRU from any chunks persisted in a prior session and
/// mirrors future writes back to IndexedDB. Returned erased as the
/// [`SwarmLocalStore`] the launcher takes.
async fn open_indexeddb_store() -> Result<Arc<dyn SwarmLocalStore>, JsValue> {
    let db = IndexedDbDatabase::open(CACHE_DB_NAME, &[IndexedDbBackend::store_name()])
        .await
        .map_err(|e| JsValue::from_str(&format!("failed to open IndexedDB cache: {e}")))?;
    let backend = IndexedDbBackend::new(db.into_arc(), DEFAULT_CACHE_BUDGET_BYTES as usize);
    let store = ChunkStore::with_backend(backend, DEFAULT_SOC_CACHE_TTL_NS, SystemClock);
    Ok(Arc::new(store))
}

/// Read the optional SWAP config from the page URL query string.
///
/// `?chequebook=0x...` enables SWAP cheque settlement; an additional `&rpc=...`
/// turns on on-chain cashout of received cheques. Returns `None` when no
/// chequebook is supplied or the address does not parse, leaving the client on
/// pseudosettle-only settlement.
fn swap_config_from_page() -> Option<LauncherSwapConfig> {
    let search = web_sys::window()?.location().search().ok()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;

    let chequebook_raw = params.get("chequebook")?;
    let chequebook = match chequebook_raw.parse::<Address>() {
        Ok(address) => address,
        Err(e) => {
            warn!(?e, "ignoring unparsable chequebook address");
            return None;
        }
    };

    let mut config = LauncherSwapConfig::new(chequebook);
    config.rpc_url = params.get("rpc").filter(|url| !url.is_empty());
    Some(config)
}

/// Apply retrieval and prefetch overrides from the page URL (`rw`, `wavestep`,
/// `stagger`, `budget`, `busy`, `pf`, `yieldn`, `pipeline`). A measurement aid for
/// sweeping the download tuning without rebuilding; absent params leave the
/// compiled defaults in place.
fn apply_retrieval_overrides_from_page() {
    let Some(search) = web_sys::window().and_then(|w| w.location().search().ok()) else {
        return;
    };
    let Ok(params) = web_sys::UrlSearchParams::new_with_str(&search) else {
        return;
    };
    let parse = |k: &str| params.get(k).and_then(|v| v.parse::<u64>().ok());
    client::configure_retrieval_race(
        parse("rw"),
        parse("wavestep"),
        parse("stagger"),
        parse("budget"),
        parse("busy"),
    );
    client::configure_prefetch(parse("pf").map(|v| v as usize));
    // The download client raises the per-peer in-flight cap above the
    // interop-conservative node default: a browser owns one neighbourhood, so the
    // tail of a large file is served by only a handful of close peers and a deeper
    // per-peer fan-out is what keeps those few peers busy. Live storers tolerate
    // it (transport resets stay a small minority of legs). Overridable via
    // `inflight` for sweeping.
    vertex_swarm_node::set_inflight_per_peer(parse("inflight").map_or(16, |v| v as usize));
    client::configure_yield_batch(parse("yieldn").map(|v| v as usize));
    client::configure_prefetch_pipeline(params.get("pipeline").is_some_and(|v| v != "0"));
    // Re-fetch the prefetch-skipped tail by default: warming those few hard,
    // deep-forwarding chunks in wide passes after the first wave drains beats
    // leaving them for the ordered joiner to grind one neighbourhood-bound
    // subtree at a time. Disable with `refetch=0`.
    client::configure_prefetch_refetch(params.get("refetch").map_or(true, |v| v != "0"));
    client::configure_load_balance(
        params.get("lb").map(|v| v != "0"),
        parse("lbtopk"),
        parse("lbhedge"),
    );
}

/// Per-bin connection dial budget for the browser download client.
///
/// Above the node default so the taper holds more peers in every balanced bin.
/// Under a wide concurrent download the closest bins drain faster than they
/// refill; a larger budget keeps the neighbourhood depth from collapsing while
/// the prefetch runs (measured: depth held for the whole download at this
/// level, versus repeated dips to zero at the default). It does not raise
/// download throughput on its own: retrieval over a light forwarding client is
/// bounded by per-chunk forwarding latency against the per-peer in-flight slot
/// budget, not by connected-peer count.
const DEMO_TOTAL_TARGET: usize = 320;

/// Build the download client's [`KademliaConfig`], applying [`DEMO_TOTAL_TARGET`]
/// by default and honouring `tt` (total connected-peer target) and `nom`
/// (per-balanced-bin floor) page-URL overrides for sweeping without a rebuild.
///
/// `nom` stuffs the shallow bins (bin 0/1/2, which cover most of the address
/// space) but in measurement did not cut the forwarded-but-absent (`Remote`)
/// miss rate: a shallow peer is still a poor entrypoint for a far chunk, so it
/// is left unset by default.
fn kademlia_config_from_page() -> Option<KademliaConfig> {
    let parse = |k: &str| {
        web_sys::window()
            .and_then(|w| w.location().search().ok())
            .and_then(|s| web_sys::UrlSearchParams::new_with_str(&s).ok())
            .and_then(|p| p.get(k))
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
    };
    let total = parse("tt").unwrap_or(DEMO_TOTAL_TARGET);
    let mut config = KademliaConfig::default().with_total_target(total);
    if let Some(nominal) = parse("nom") {
        config = config.with_nominal(nominal);
    }
    Some(config)
}

/// Publish a running demo handle on `window.__swarmDemo` for the JS frontend.
fn publish_handle(demo: SwarmDemo) {
    let handle: JsValue = demo.into();
    match web_sys::window() {
        Some(window) => {
            if js_sys::Reflect::set(&window, &JsValue::from_str("__swarmDemo"), &handle).is_err() {
                // The global was not set; leak the JS handle so its node tasks
                // still run for the session even without the live peer feed.
                std::mem::forget(handle);
            }
        }
        None => std::mem::forget(handle),
    }
}

/// Install the browser tracing subscriber, quiet by default; `?trace`/`?debug`
/// in the page URL raise verbosity.
pub(crate) fn init_tracing() {
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    // The console layer itself passes everything at/under TRACE; the `Targets`
    // filter below is what actually decides what is shown.
    let wasm_layer = tracing_wasm::WASMLayer::new(
        tracing_wasm::WASMLayerConfigBuilder::new()
            .set_max_level(Level::TRACE)
            .build(),
    );

    let filter = match tracing_verbosity_override() {
        // `?trace`: full firehose.
        Some(LevelFilter::TRACE) => Targets::new().with_default(LevelFilter::TRACE),
        // `?verbose` / `?debug`: everything at DEBUG.
        Some(level) => Targets::new().with_default(level),
        // Default: app-level INFO, networking/retrieval internals at WARN.
        None => Targets::new()
            .with_default(LevelFilter::INFO)
            // vertex networking / retrieval / topology / handshake internals.
            .with_target("vertex_swarm_node", LevelFilter::WARN)
            .with_target("vertex_swarm_topology", LevelFilter::WARN)
            .with_target("vertex_swarm_stream", LevelFilter::WARN)
            .with_target("vertex_net_codec", LevelFilter::WARN)
            .with_target("vertex_net_dialer", LevelFilter::WARN)
            .with_target("vertex_net_ratelimiter", LevelFilter::WARN)
            // libp2p stack + transport plumbing.
            .with_target("libp2p", LevelFilter::WARN)
            .with_target("libp2p_swarm", LevelFilter::WARN)
            .with_target("libp2p_core", LevelFilter::WARN)
            .with_target("libp2p_yamux", LevelFilter::WARN)
            .with_target("libp2p_websocket_websys", LevelFilter::WARN)
            .with_target("yamux", LevelFilter::WARN)
            .with_target("multistream_select", LevelFilter::WARN),
    };

    use tracing_subscriber::Layer;
    // `try_init`: a second call must not panic on an already-set global
    // subscriber (the worker entry also initialises tracing).
    let _ = tracing_subscriber::registry()
        .with(wasm_layer.with_filter(filter))
        .try_init();
}

/// Read a one-shot verbosity override (`?trace`/`?verbose`/`?debug`) from the page URL.
fn tracing_verbosity_override() -> Option<tracing_subscriber::filter::LevelFilter> {
    use tracing_subscriber::filter::LevelFilter;

    let search = web_sys::window()?.location().search().ok()?.to_lowercase();
    if search.contains("trace") {
        Some(LevelFilter::TRACE)
    } else if search.contains("verbose") || search.contains("debug") {
        Some(LevelFilter::DEBUG)
    } else {
        None
    }
}

/// Per-reason disconnect tally over the session, surfaced as a periodic console
/// histogram so the harness can attribute a peer-set collapse to its cause
/// (remote/transport close vs local bin-trim vs orderly local close).
/// Measurement aid for the download peer-timeline investigation.
static DISCONNECT_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DISCONNECT_BIN_TRIMMED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DISCONNECT_CONN_ERROR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DISCONNECT_LOCAL_CLOSE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn record_disconnect(reason: &vertex_swarm_topology::DisconnectReason) {
    use std::sync::atomic::Ordering::Relaxed;
    use vertex_swarm_topology::DisconnectReason;
    DISCONNECT_TOTAL.fetch_add(1, Relaxed);
    match reason {
        DisconnectReason::BinTrimmed => DISCONNECT_BIN_TRIMMED.fetch_add(1, Relaxed),
        DisconnectReason::ConnectionError => DISCONNECT_CONN_ERROR.fetch_add(1, Relaxed),
        DisconnectReason::LocalClose => DISCONNECT_LOCAL_CLOSE.fetch_add(1, Relaxed),
    };
    let total = DISCONNECT_TOTAL.load(Relaxed);
    if total.is_multiple_of(10) {
        tracing::info!(
            "disconnect-histogram total={total} bin_trimmed={} conn_error={} local_close={}",
            DISCONNECT_BIN_TRIMMED.load(Relaxed),
            DISCONNECT_CONN_ERROR.load(Relaxed),
            DISCONNECT_LOCAL_CLOSE.load(Relaxed),
        );
    }
}

/// Forward topology events from the broadcast subscription to both consumers.
fn spawn_event_pump(
    topology: TopologyHandle<Arc<Identity>>,
    events: std::rc::Rc<std::cell::RefCell<VecDeque<TopologyEvent>>>,
    peer_events: std::rc::Rc<std::cell::RefCell<VecDeque<PeerTrigger>>>,
) {
    use tokio::sync::broadcast::error::RecvError;

    wasm_bindgen_futures::spawn_local(async move {
        let mut rx = topology.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let TopologyEvent::PeerDisconnected { reason, .. } = &event {
                        record_disconnect(reason);
                    }
                    let (kind, detail) = describe_event(&event);
                    ui::append_event(kind, &escape_html(&detail));

                    // Capture peer connect/disconnect as overlay-keyed triggers
                    // for the globe feed, enriched against the peer manager when
                    // `drain_peer_events` is called.
                    if let Some(trigger) = peer_trigger(&event) {
                        let mut pbuf = peer_events.borrow_mut();
                        if pbuf.len() >= PEER_EVENT_BUFFER_CAP {
                            pbuf.pop_front();
                        }
                        pbuf.push_back(trigger);
                    }

                    let mut buf = events.borrow_mut();
                    if buf.len() >= EVENT_BUFFER_CAP {
                        buf.pop_front();
                    }
                    buf.push_back(event);
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Map a topology event to a globe peer trigger, if it is one.
fn peer_trigger(event: &TopologyEvent) -> Option<PeerTrigger> {
    match event {
        TopologyEvent::PeerReady {
            overlay,
            peer_id,
            node_type,
            ..
        } => Some(PeerTrigger::Connect {
            overlay: *overlay,
            peer_id: *peer_id,
            node_type: *node_type,
        }),
        TopologyEvent::PeerDisconnected { overlay, .. } => {
            Some(PeerTrigger::Disconnect { overlay: *overlay })
        }
        _ => None,
    }
}

/// Drive the topology UI on a one-second interval.
fn spawn_render_loop(topology: TopologyHandle<Arc<Identity>>) {
    use gloo_timers::future::TimeoutFuture;

    wasm_bindgen_futures::spawn_local(async move {
        loop {
            render_once(&topology);
            TimeoutFuture::new(1_000).await;
        }
    });
}

/// Render a single frame: the status line, stats grid, and per-bin table.
fn render_once(topology: &TopologyHandle<Arc<Identity>>) {
    let snap = topology.readiness();

    if snap.connected_peers == 0 {
        ui::set_status("connecting...");
    } else if snap.is_routable() {
        ui::set_status("connected, building topology");
    } else {
        ui::set_status("connected to peers, awaiting a storer");
    }

    ui::render_stats(&ui::Stats {
        connected_peers: snap.connected_peers as u32,
        connected_storers: snap.connected_storers as u32,
        depth: u32::from(snap.depth.get()),
        neighborhood_connected: snap.neighborhood_connected as u32,
        saturation_threshold: snap.saturation_threshold as u32,
        bins_at_target: snap.bins_at_target as u32,
        phase: snap.phase.into(),
        is_routable: snap.is_routable(),
        is_saturated: snap.is_saturated(),
    });

    let rows: Vec<(u32, u32, Option<u32>, u32)> = snap
        .bins
        .iter()
        .map(|b| {
            (
                u32::from(b.bin.get()),
                b.connected as u32,
                b.target.map(|t| t as u32),
                b.deficit as u32,
            )
        })
        .collect();
    ui::render_bins(&rows);
}

/// Escape the HTML-special characters in event detail text.
fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// A short label and a human-readable summary for a topology event.
fn describe_event(event: &TopologyEvent) -> (&'static str, String) {
    match event {
        TopologyEvent::PeerReady {
            overlay, node_type, ..
        } => (
            "peer_ready",
            format!("{node_type} {}", short_overlay(&overlay.to_string())),
        ),
        TopologyEvent::PeerRejected {
            overlay, reason, ..
        } => (
            "peer_rejected",
            format!("{} ({reason:?})", short_overlay(&overlay.to_string())),
        ),
        TopologyEvent::PeerDisconnected {
            overlay, reason, ..
        } => (
            "peer_disconnected",
            format!("{} ({reason:?})", short_overlay(&overlay.to_string())),
        ),
        TopologyEvent::DepthChanged {
            old_depth,
            new_depth,
        } => ("depth_changed", format!("{old_depth} -> {new_depth}")),
        TopologyEvent::PhaseChanged { from, to, depth } => {
            ("phase_changed", format!("{from} -> {to} (depth {depth})"))
        }
        TopologyEvent::DialFailed { overlay, error, .. } => (
            "dial_failed",
            match overlay {
                Some(o) => format!("{} ({error})", short_overlay(&o.to_string())),
                None => format!("{error}"),
            },
        ),
        TopologyEvent::PingCompleted { overlay, rtt } => (
            "ping",
            format!(
                "{} {}ms",
                short_overlay(&overlay.to_string()),
                rtt.as_millis()
            ),
        ),
    }
}

/// Abbreviate a hex overlay address to its first and last few characters.
fn short_overlay(overlay: &str) -> String {
    let trimmed = overlay.strip_prefix("0x").unwrap_or(overlay);
    if trimmed.len() <= 12 {
        return trimmed.to_string();
    }
    format!("{}..{}", &trimmed[..6], &trimmed[trimmed.len() - 4..])
}

fn set_u32(obj: &js_sys::Object, key: &str, value: u32) {
    set_value(obj, key, &JsValue::from_f64(f64::from(value)));
}

fn set_bool(obj: &js_sys::Object, key: &str, value: bool) {
    set_value(obj, key, &JsValue::from_bool(value));
}

fn set_str(obj: &js_sys::Object, key: &str, value: &str) {
    set_value(obj, key, &JsValue::from_str(value));
}

fn set_null(obj: &js_sys::Object, key: &str) {
    set_value(obj, key, &JsValue::NULL);
}

fn set_value(obj: &js_sys::Object, key: &str, value: &JsValue) {
    // The key is a static identifier and the object is freshly constructed, so
    // the reflect set cannot fail; ignore the result.
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), value);
}
