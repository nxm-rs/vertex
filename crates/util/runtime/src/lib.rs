//! Cfg-gated foundation for wall-clock time and randomness.
//!
//! This crate is the single home for the wasm/native split of two
//! cross-cutting concerns:
//!
//! - [`time`]: wall-clock and monotonic time. The surface re-exports
//!   `web-time`, which is a `std::time` re-export on native targets and the
//!   browser clock on `wasm32`, so callers get one import path and the platform
//!   choice lives in exactly one dependency.
//! - [`rand`]: a getrandom-backed randomness facade that never touches a
//!   thread-local generator, so it builds and runs under the
//!   `getrandom_backend="wasm_js"` configuration used for browser targets.
//!
//! Centralising both here means downstream crates do not each repeat the
//! `cfg(target_arch = "wasm32")` plumbing or the
//! `SystemTime::now().duration_since(UNIX_EPOCH)` boilerplate. The crate builds
//! for `wasm32-unknown-unknown` and that is enforced in CI.
//!
//! # What this crate does not own
//!
//! - The async runtime. Task spawning (`tokio::spawn` versus
//!   `wasm_bindgen_futures::spawn_local`) lives in the task-executor layer, not
//!   here.
//! - Timers. `sleep`, `interval`, `timeout`, and the timer-coherent `Instant`
//!   (tokio's pause-aware clock on native, the browser clock on wasm32) live in
//!   `vertex_tasks::time`; this crate only provides the real platform wall and
//!   monotonic clocks for timer-free code.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// On wasm32 the randomness facade reaches `getrandom` 0.3 through `rand`'s
// entropy source. The `getrandom_03` dependency exists only to select that
// crate's `wasm_js` browser backend feature; it is never named in source, so
// reference it here to satisfy the unused-crate-dependencies lint.
#[cfg(target_arch = "wasm32")]
use getrandom_03 as _;

pub mod rand;
pub mod time;
