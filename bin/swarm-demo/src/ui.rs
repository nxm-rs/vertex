//! Minimal DOM UI for the topology demo, driven through `web-sys`.
//!
//! [`mount`] builds the static panel once (header, status line, a stats grid, a
//! per-bin table, and a scrolling event log). The render helpers
//! ([`set_status`], [`render_stats`], [`render_bins`], [`append_event`]) update
//! the live regions by id. Keeping the structure static and updating text and
//! rows in place avoids rebuilding the tree on every one-second tick.

use web_sys::{Document, Element};

/// Element id of the status line.
const STATUS_ID: &str = "status";
/// Element id of the stats grid container.
const STATS_ID: &str = "stats";
/// Element id of the per-bin table body.
const BINS_ID: &str = "bins";
/// Element id of the scrolling event log container.
const LOG_ID: &str = "log";
/// Maximum log rows kept in the DOM before the oldest are trimmed.
const MAX_LOG_ROWS: usize = 200;

/// The document, or a panic: the app only runs in a browser with a document.
fn document() -> Document {
    web_sys::window()
        .and_then(|w| w.document())
        .expect("browser document is available")
}

fn by_id(id: &str) -> Option<Element> {
    document().get_element_by_id(id)
}

/// Build the static panel and mount it into the document body.
///
/// `overlay` is the node's ephemeral overlay address, shown in the header so the
/// session identity is visible.
pub fn mount(overlay: &str) {
    let doc = document();
    let body = doc.body().expect("document has a body");

    let root = doc.create_element("div").expect("create div");
    root.set_class_name("panel");
    root.set_inner_html(&format!(
        "<h1>Vertex Swarm browser client</h1>\
         <p class=\"overlay\">overlay <code>{overlay}</code></p>\
         <p id=\"{STATUS_ID}\" class=\"status\">starting...</p>\
         <div id=\"{STATS_ID}\" class=\"stats\"></div>\
         <h2>Bins</h2>\
         <table class=\"bins\"><thead><tr>\
           <th>bin</th><th>connected</th><th>target</th><th>deficit</th>\
         </tr></thead><tbody id=\"{BINS_ID}\"></tbody></table>\
         <h2>Events</h2>\
         <div id=\"{LOG_ID}\" class=\"log\"></div>"
    ));

    body.append_child(&root).expect("append panel");
}

/// Update the status line text.
pub fn set_status(text: &str) {
    if let Some(el) = by_id(STATUS_ID) {
        el.set_text_content(Some(text));
    }
}

/// The headline topology counters rendered in the stats grid.
pub struct Stats<'a> {
    /// Total connected peers.
    pub connected_peers: u32,
    /// Connected peers whose handshake-confirmed type stores chunks.
    pub connected_storers: u32,
    /// Neighborhood depth.
    pub depth: u32,
    /// Connected peers in bins at or beyond the depth boundary.
    pub neighborhood_connected: u32,
    /// Per-bin saturation target the neighborhood aims for.
    pub saturation_threshold: u32,
    /// Bins with a finite target whose connected count meets it.
    pub bins_at_target: u32,
    /// Current topology phase label.
    pub phase: &'a str,
    /// Whether a push or retrieval has a storer to ask.
    pub is_routable: bool,
    /// Whether the neighborhood is saturated.
    pub is_saturated: bool,
}

/// Render the headline counters: connected peers, storers, depth, phase, and
/// the saturation and routability flags.
pub fn render_stats(stats: &Stats) {
    let Some(el) = by_id(STATS_ID) else {
        return;
    };
    let routable = if stats.is_routable { "yes" } else { "no" };
    let saturated = if stats.is_saturated { "yes" } else { "no" };
    let Stats {
        connected_peers,
        connected_storers,
        depth,
        neighborhood_connected,
        saturation_threshold,
        bins_at_target,
        phase,
        ..
    } = *stats;
    el.set_inner_html(&format!(
        "<div class=\"stat\"><span class=\"k\">peers</span><span class=\"v\">{connected_peers}</span></div>\
         <div class=\"stat\"><span class=\"k\">storers</span><span class=\"v\">{connected_storers}</span></div>\
         <div class=\"stat\"><span class=\"k\">depth</span><span class=\"v\">{depth}</span></div>\
         <div class=\"stat\"><span class=\"k\">phase</span><span class=\"v\">{phase}</span></div>\
         <div class=\"stat\"><span class=\"k\">neighborhood</span><span class=\"v\">{neighborhood_connected} / {saturation_threshold}</span></div>\
         <div class=\"stat\"><span class=\"k\">bins at target</span><span class=\"v\">{bins_at_target}</span></div>\
         <div class=\"stat\"><span class=\"k\">routable</span><span class=\"v\">{routable}</span></div>\
         <div class=\"stat\"><span class=\"k\">saturated</span><span class=\"v\">{saturated}</span></div>"
    ));
}

/// Replace the per-bin table body with the current per-bin fill.
///
/// `rows` is `(bin, connected, target, deficit)` where `target` is `None` for
/// neighborhood bins, which connect to every available peer.
pub fn render_bins(rows: &[(u32, u32, Option<u32>, u32)]) {
    let Some(el) = by_id(BINS_ID) else {
        return;
    };
    let mut html = String::new();
    for (bin, connected, target, deficit) in rows {
        let target_cell = match target {
            Some(t) => t.to_string(),
            None => "all".to_string(),
        };
        let filled = target.is_none() || *deficit == 0;
        let class = if filled { "filled" } else { "" };
        html.push_str(&format!(
            "<tr class=\"{class}\"><td>{bin}</td><td>{connected}</td><td>{target_cell}</td><td>{deficit}</td></tr>"
        ));
    }
    el.set_inner_html(&html);
}

/// Append one event row to the scrolling log, trimming the oldest rows.
pub fn append_event(kind: &str, detail: &str) {
    let Some(log) = by_id(LOG_ID) else {
        return;
    };
    let doc = document();
    let row = doc.create_element("div").expect("create row");
    row.set_class_name(&format!("event {kind}"));
    let row_el: &Element = &row;
    row_el.set_inner_html(&format!(
        "<span class=\"kind\">{kind}</span><span class=\"detail\">{detail}</span>"
    ));
    log.append_child(&row).expect("append event row");

    while log.child_element_count() as usize > MAX_LOG_ROWS {
        if let Some(first) = log.first_element_child() {
            let _ = log.remove_child(&first);
        } else {
            break;
        }
    }

    log.set_scroll_top(log.scroll_height());
}
