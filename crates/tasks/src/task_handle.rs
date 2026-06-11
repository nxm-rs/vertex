//! Platform-gated handle returned by every [`TaskExecutor`](crate::TaskExecutor) spawner.
//!
//! On native targets a [`TaskHandle`] is exactly a [`tokio::task::JoinHandle<()>`], so the
//! native public API is byte-identical to spawning directly onto the tokio runtime: callers can
//! `.await` the handle to observe completion and call `abort()` to cancel.
//!
//! On `wasm32` there is no multi-thread tokio runtime and no [`JoinHandle`](tokio::task::JoinHandle).
//! Tasks run on the browser event loop via [`wasm_bindgen_futures::spawn_local`], which neither
//! yields a join future nor a cancellation token. [`TaskHandle`] is therefore a small abortable
//! wrapper around a [`futures_util::future::AbortHandle`]: `abort()` cancels the underlying task,
//! and the type is otherwise inert. It deliberately does not implement [`Future`](core::future::Future),
//! because a wasm task cannot be joined.

/// Native [`TaskHandle`]: a re-export of [`tokio::task::JoinHandle<()>`].
///
/// The native spawners return this directly, keeping the native API identical to spawning onto
/// the tokio runtime.
#[cfg(not(target_arch = "wasm32"))]
pub type TaskHandle = tokio::task::JoinHandle<()>;

/// Wasm [`TaskHandle`]: an abortable wrapper over a [`futures_util::future::AbortHandle`].
///
/// A task spawned with [`wasm_bindgen_futures::spawn_local`] cannot be joined, so this handle only
/// supports cancellation. Dropping the handle does not cancel the task; call [`abort`](Self::abort)
/// explicitly.
#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone)]
pub struct TaskHandle {
    abort: futures_util::future::AbortHandle,
}

#[cfg(target_arch = "wasm32")]
impl TaskHandle {
    /// Wraps an [`AbortHandle`](futures_util::future::AbortHandle) for the spawned task.
    pub(crate) const fn new(abort: futures_util::future::AbortHandle) -> Self {
        Self { abort }
    }

    /// Aborts the spawned task. Has no effect if the task already finished.
    pub fn abort(&self) {
        self.abort.abort();
    }
}
