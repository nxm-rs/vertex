//! Browser Swarm client demo.
//!
//! This wasm-bindgen app mints an ephemeral Swarm client identity, resolves the
//! live mainnet bootnodes over DNS-over-HTTPS (falling back to an embedded
//! snapshot), starts a browser client node over secure websockets, and renders
//! its Kademlia topology building up: connected peer count, neighborhood depth,
//! per-bin fill, the topology phase, and a scrolling log of topology events.
//!
//! The exported surface is small and JS-facing:
//!
//! - [`start`] is the entrypoint: it installs the panic hook and console
//!   tracing, builds the client, and returns a [`SwarmDemo`] handle.
//! - [`SwarmDemo::readiness`] returns a plain JS object with the current
//!   readiness snapshot fields, polled by the UI on an interval.
//! - [`SwarmDemo::drain_events`] returns the topology events seen since the last
//!   call as an array of JS objects, so the UI can append them to its log.
//!
//! The UI itself is driven from this module via `web-sys` so the app is a single
//! wasm artifact with a minimal HTML shell.

mod ui;

use std::collections::VecDeque;
use std::sync::Arc;

use alloy_primitives::Address;
use tracing::{info, warn};
use vertex_net_dnsaddr_doh::{DohClient, resolve_mainnet_wss_bootnodes};
use vertex_swarm_api::{SwarmIdentity, SwarmLocalStore};
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::{
    ChunkStore, DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_SOC_CACHE_TTL_NS, IndexedDbBackend, SystemClock,
};
use vertex_swarm_node::{ClientLauncher, LauncherSwapConfig, SwarmNodeType};
use vertex_swarm_spec::{init_mainnet, mainnet_wss_bootnodes};
use vertex_swarm_topology::{TopologyEvent, TopologyHandle};
use vertex_storage_indexeddb::IndexedDbDatabase;
use vertex_tasks::TaskManager;
use wasm_bindgen::prelude::*;

/// IndexedDB database name for the browser client cache.
const CACHE_DB_NAME: &str = "vertex-swarm-cache";

/// Maximum topology events buffered between UI drains.
///
/// The UI drains on a one-second interval; a burst beyond this drops the oldest
/// events, which only affects the scrolling log, never the live counters (those
/// come from [`SwarmDemo::readiness`]).
const EVENT_BUFFER_CAP: usize = 256;

/// A running browser Swarm client, exposed to JS.
///
/// Holds the [`TopologyHandle`] for the live node plus a rolling buffer of
/// recent [`TopologyEvent`]s. The node tasks run on the browser event loop; this
/// handle is the read surface the UI polls.
#[wasm_bindgen]
pub struct SwarmDemo {
    topology: TopologyHandle<Arc<Identity>>,
    events: std::rc::Rc<std::cell::RefCell<VecDeque<TopologyEvent>>>,
    overlay: String,
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

    /// Snapshot the current readiness state as a plain JS object.
    ///
    /// Fields: `connectedPeers`, `connectedStorers`, `depth`,
    /// `neighborhoodConnected`, `saturationThreshold`, `binsAtTarget`, `phase`
    /// (a string), `isRoutable`, `isSaturated`, and `bins` (an array of
    /// `{ bin, connected, target, deficit }`, where `target` is `null` for
    /// neighborhood bins).
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

    /// Drain the topology events seen since the last call.
    ///
    /// Returns a JS array of `{ kind, detail }` objects, oldest first, and
    /// clears the buffer. `kind` is a short event label; `detail` is a
    /// human-readable summary.
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
}

/// wasm entrypoint: start the client and keep the handle alive for the session.
///
/// Trunk's bootstrap calls this exported `main` automatically. It spawns
/// [`start`] on the browser event loop and leaks the returned [`SwarmDemo`] so
/// the node tasks keep running; the page session owns it for its lifetime.
#[wasm_bindgen(start)]
pub fn main() {
    wasm_bindgen_futures::spawn_local(async {
        match start().await {
            Ok(demo) => {
                // Keep the client alive for the page session.
                std::mem::forget(demo);
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
/// Installs the panic hook and console tracing, mints an ephemeral mainnet
/// client identity, resolves the live mainnet bootnodes over DNS-over-HTTPS
/// (with the embedded wss snapshot as fallback), starts the client node, mounts
/// the UI into the document, and begins the one-second poll loop. Returns the
/// [`SwarmDemo`] handle, which keeps the node alive for the page session.
///
/// # Errors
///
/// Returns a JS error if the client node fails to build or start. Bootnode
/// resolution never fails: it always falls back to the embedded snapshot.
#[wasm_bindgen]
pub async fn start() -> Result<SwarmDemo, JsValue> {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

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

    let client = launcher
        .launch()
        .await
        .map_err(|e| JsValue::from_str(&format!("failed to launch client: {e}")))?;
    let topology = client.topology().clone();

    let events = std::rc::Rc::new(std::cell::RefCell::new(VecDeque::with_capacity(
        EVENT_BUFFER_CAP,
    )));
    spawn_event_pump(topology.clone(), events.clone());
    spawn_render_loop(topology.clone());

    ui::set_status("connecting...");

    let demo = SwarmDemo {
        topology,
        events,
        overlay,
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

/// Forward topology events from the broadcast subscription to both consumers.
///
/// Each event is appended to the scrolling DOM log (the live UI) and pushed onto
/// the `events` buffer that the JS [`SwarmDemo::drain_events`] accessor reads, so
/// the two consumers never compete for the same event. Runs on the browser event
/// loop for the session. The subscription can lag under a burst; a lagged
/// receiver skips ahead and keeps going rather than stalling, since the live
/// counters come from `readiness`, not this stream.
fn spawn_event_pump(
    topology: TopologyHandle<Arc<Identity>>,
    events: std::rc::Rc<std::cell::RefCell<VecDeque<TopologyEvent>>>,
) {
    use tokio::sync::broadcast::error::RecvError;

    wasm_bindgen_futures::spawn_local(async move {
        let mut rx = topology.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let (kind, detail) = describe_event(&event);
                    ui::append_event(kind, &escape_html(&detail));

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

/// Drive the topology UI on a one-second interval.
///
/// Each tick snapshots readiness into the stats grid and per-bin table. The
/// scrolling event log is updated by the event pump as events arrive, not here.
/// Runs on the browser event loop for the session.
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
///
/// Event details embed peer-supplied data (overlay addresses, error strings), so
/// they are escaped before going into `innerHTML`.
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
