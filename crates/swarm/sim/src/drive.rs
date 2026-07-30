//! Multi-source drive loop sharing the seeded harness's vocabulary.
//!
//! Sim-side driving must never draw branch order from tokio's unseeded RNG,
//! so this loop is the biased-select twin of the harness driver; port a
//! harness test by swapping the import.

use std::future::poll_fn;
use std::task::Poll;
use std::time::Duration;

pub use vertex_swarm_test_utils::harness::DrivableSwarm;

/// Drive every source in `drivables`, calling `hook` after each poll round,
/// until the hook yields a value or `timeout` of virtual time elapses;
/// returns the hook's value, or `None` on timeout.
pub async fn drive<T>(
    drivables: &mut [&mut dyn DrivableSwarm],
    timeout: Duration,
    mut hook: impl FnMut() -> Option<T>,
) -> Option<T> {
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        if let Some(value) = hook() {
            return Some(value);
        }
        let step = poll_fn(|cx| {
            let mut progress = Poll::Pending;
            for drivable in drivables.iter_mut() {
                if drivable.poll_drive(cx).is_ready() {
                    progress = Poll::Ready(());
                }
            }
            progress
        });
        tokio::select! {
            biased;
            () = &mut deadline => return hook(),
            () = step => {}
        }
    }
}
