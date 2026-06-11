//! Background tasks for topology infrastructure (browser build).
//!
//! The browser has no network interfaces to enumerate, so the interface watcher
//! that drives subnet discovery on native targets is a no-op here. A wasm client
//! dials over a websocket transport and never classifies local subnets.

use vertex_tasks::TaskExecutor;

/// No-op interface watcher for the browser build.
///
/// On native targets this subscribes to netlink address events to discover the
/// local subnets. The browser exposes no such interface, so there is nothing to
/// watch and the function returns immediately.
pub(crate) fn spawn_interface_watcher(_executor: &TaskExecutor) {}
