//! Background tasks for topology infrastructure.

use vertex_tasks::TaskExecutor;

/// Spawn a background task that watches network interface changes for subnet discovery.
///
/// Subscribes to netlink address events via `if-watch` and fires initial `Up` events
/// for all existing addresses, then ongoing `Up`/`Down` as interfaces change.
pub(crate) fn spawn_interface_watcher(executor: &TaskExecutor) {
    executor.spawn_with_graceful_shutdown_signal(
        "net.interface_watcher",
        move |shutdown| async move {
            use futures::StreamExt;

            let mut watcher = match if_watch::tokio::IfWatcher::new() {
                Ok(w) => w,
                Err(e) => {
                    tracing::error!(error = %e, "failed to create interface watcher");
                    return;
                }
            };

            let mut shutdown = std::pin::pin!(shutdown);
            loop {
                tokio::select! {
                    guard = &mut shutdown => {
                        drop(guard);
                        break;
                    }
                    event = watcher.next() => {
                        match event {
                            Some(Ok(if_watch::IfEvent::Up(net))) => {
                                vertex_net_local::add_subnet(net);
                            }
                            Some(Ok(if_watch::IfEvent::Down(net))) => {
                                vertex_net_local::remove_subnet(net);
                            }
                            Some(Err(e)) => {
                                tracing::warn!(error = %e, "interface watcher error");
                            }
                            None => break,
                        }
                    }
                }
            }
        },
    );
}
