//! Vertex task management.
//!
//! This crate provides centralized task lifecycle management for Vertex,
//! following patterns from reth-tasks.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use std::{
    any::Any,
    fmt::{Display, Formatter},
    pin::{Pin, pin},
    sync::{Arc, OnceLock},
    task::{Context, Poll, ready},
};

use dyn_clone::DynClone;
use futures_util::{
    Future, FutureExt, TryFutureExt,
    future::{BoxFuture, Either, select},
};
use tokio::{
    runtime::Handle,
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinHandle,
};
use tracing::{debug, error, info, warn};
use vertex_metrics::OperationGuard;

pub mod metrics;
pub mod shutdown;

use crate::metrics::TaskExecutorMetrics;
use crate::shutdown::GracefulShutdownCounter;

// Re-export key types
pub use shutdown::{GracefulShutdown, GracefulShutdownGuard, Shutdown, Signal, signal};

/// A boxed future representing a node's main event loop.
pub type NodeTask = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A function that creates a node task with graceful shutdown support.
///
/// Takes a [`GracefulShutdown`] signal and returns the task future.
/// When the shutdown signal fires, the task should clean up and exit.
pub type NodeTaskFn = Box<dyn FnOnce(GracefulShutdown) -> NodeTask + Send>;

/// A service that can be spawned as a background task with graceful shutdown support.
///
/// Implement this for services that run continuously (event loops, handlers, etc.)
/// and need to be spawned onto a task executor with proper shutdown handling.
pub trait SpawnableTask: Send + 'static {
    /// Consume self and return a future to run as a background task.
    ///
    /// The service should listen for the shutdown signal and exit gracefully when received.
    fn into_task(self, shutdown: GracefulShutdown) -> impl Future<Output = ()> + Send;
}

/// Global [`TaskExecutor`] instance that can be accessed from anywhere.
static GLOBAL_EXECUTOR: OnceLock<TaskExecutor> = OnceLock::new();

/// A type that can spawn tasks.
///
/// The main purpose of this type is to abstract over [`TaskExecutor`] so it's more convenient to
/// provide default impls for testing.
///
/// # Examples
///
/// Use the [`TokioTaskExecutor`] that spawns with [`tokio::task::spawn`]
///
/// ```
/// # async fn t() {
/// use vertex_tasks::{TaskSpawner, TokioTaskExecutor};
/// let executor = TokioTaskExecutor::default();
///
/// let task = executor.spawn(Box::pin(async {
///     // -- snip --
/// }));
/// task.await.unwrap();
/// # }
/// ```
///
/// Use the [`TaskExecutor`] that spawns task directly onto the tokio runtime via the [Handle].
///
/// ```
/// # use vertex_tasks::TaskManager;
/// fn t() {
///  use vertex_tasks::TaskSpawner;
/// let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
/// let manager = TaskManager::new(rt.handle().clone());
/// let executor = manager.executor();
/// let task = TaskSpawner::spawn(&executor, Box::pin(async {
///     // -- snip --
/// }));
/// rt.block_on(task).unwrap();
/// # }
/// ```
///
/// The [`TaskSpawner`] trait is [`DynClone`] so `Box<dyn TaskSpawner>` are also `Clone`.
#[auto_impl::auto_impl(&, Arc)]
pub trait TaskSpawner: Send + Sync + Unpin + std::fmt::Debug + DynClone {
    /// Spawns the task onto the runtime.
    /// See also [`Handle::spawn`].
    fn spawn(&self, fut: BoxFuture<'static, ()>) -> JoinHandle<()>;

    /// This spawns a critical task onto the runtime.
    fn spawn_critical(&self, name: &'static str, fut: BoxFuture<'static, ()>) -> JoinHandle<()>;

    /// Spawns a blocking task onto the runtime.
    fn spawn_blocking(&self, name: &'static str, fut: BoxFuture<'static, ()>) -> JoinHandle<()>;

    /// This spawns a critical blocking task onto the runtime.
    fn spawn_critical_blocking(
        &self,
        name: &'static str,
        fut: BoxFuture<'static, ()>,
    ) -> JoinHandle<()>;
}

dyn_clone::clone_trait_object!(TaskSpawner);

/// An [`TaskSpawner`] that uses [`tokio::task::spawn`] to execute tasks
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TokioTaskExecutor;

impl TokioTaskExecutor {
    /// Converts the instance to a boxed [`TaskSpawner`].
    pub fn boxed(self) -> Box<dyn TaskSpawner + 'static> {
        Box::new(self)
    }
}

impl TaskSpawner for TokioTaskExecutor {
    fn spawn(&self, fut: BoxFuture<'static, ()>) -> JoinHandle<()> {
        tokio::task::spawn(fut)
    }

    fn spawn_critical(&self, _name: &'static str, fut: BoxFuture<'static, ()>) -> JoinHandle<()> {
        tokio::task::spawn(fut)
    }

    fn spawn_blocking(&self, _name: &'static str, fut: BoxFuture<'static, ()>) -> JoinHandle<()> {
        tokio::task::spawn_blocking(move || tokio::runtime::Handle::current().block_on(fut))
    }

    fn spawn_critical_blocking(
        &self,
        _name: &'static str,
        fut: BoxFuture<'static, ()>,
    ) -> JoinHandle<()> {
        tokio::task::spawn_blocking(move || tokio::runtime::Handle::current().block_on(fut))
    }
}

/// Many vertex components require to spawn tasks for long-running jobs. For example network
/// spawns tasks to handle egress and ingress of traffic or topology that spawns tasks
/// that manage peer connections.
///
/// To unify how tasks are created, the [`TaskManager`] provides access to the configured Tokio
/// runtime. A [`TaskManager`] stores the [`tokio::runtime::Handle`] it is associated with. In this
/// way it is possible to configure on which runtime a task is executed.
///
/// The main purpose of this type is to be able to monitor if a critical task panicked, for
/// diagnostic purposes, since tokio task essentially fail silently. Therefore, this type is a
/// Stream that yields the name of panicked task, See [`TaskExecutor::spawn_critical`]. In order to
/// execute Tasks use the [`TaskExecutor`] type [`TaskManager::executor`].
#[derive(Debug)]
#[must_use = "TaskManager must be polled to monitor critical tasks"]
pub struct TaskManager {
    /// Handle to the tokio runtime this task manager is associated with.
    handle: Handle,
    /// Sender half for sending task events to this type
    task_events_tx: UnboundedSender<TaskEvent>,
    /// Receiver for task events
    task_events_rx: UnboundedReceiver<TaskEvent>,
    /// The [Signal] to fire when all tasks should be shutdown.
    ///
    /// This is fired when dropped.
    signal: Option<Signal>,
    /// Receiver of the shutdown signal.
    on_shutdown: Shutdown,
    /// How many [`GracefulShutdown`] tasks are currently active
    graceful_tasks: Arc<GracefulShutdownCounter>,
}

impl TaskManager {
    /// Returns a __new__ [`TaskManager`] over the currently running Runtime.
    ///
    /// This must be polled for the duration of the program.
    ///
    /// To obtain the current [`TaskExecutor`] see [`TaskExecutor::current`].
    ///
    /// # Panics
    ///
    /// This will panic if called outside the context of a Tokio runtime.
    pub fn current() -> Self {
        let handle = Handle::current();
        Self::new(handle)
    }

    /// Create a new instance connected to the given handle's tokio runtime.
    ///
    /// This also sets the global [`TaskExecutor`].
    pub fn new(handle: Handle) -> Self {
        let (task_events_tx, task_events_rx) = unbounded_channel();
        let (signal, on_shutdown) = signal();
        let manager = Self {
            handle,
            task_events_tx,
            task_events_rx,
            signal: Some(signal),
            on_shutdown,
            graceful_tasks: Arc::new(GracefulShutdownCounter::new()),
        };

        let _ = GLOBAL_EXECUTOR
            .set(manager.executor())
            .inspect_err(|_| warn!("Global executor already set; TaskExecutor::current() will return the first instance"));

        info!("TaskManager initialized");
        manager
    }

    /// Returns a new [`TaskExecutor`] that can spawn new tasks onto the tokio runtime this type is
    /// connected to.
    pub fn executor(&self) -> TaskExecutor {
        TaskExecutor {
            handle: self.handle.clone(),
            on_shutdown: self.on_shutdown.clone(),
            task_events_tx: self.task_events_tx.clone(),
            metrics: Default::default(),
            graceful_tasks: Arc::clone(&self.graceful_tasks),
        }
    }

    /// Fires the shutdown signal and awaits until all tasks are shutdown.
    pub fn graceful_shutdown(self) {
        let _ = self.do_graceful_shutdown(None);
    }

    /// Fires the shutdown signal and awaits until all tasks are shutdown.
    ///
    /// Returns true if all tasks were shutdown before the timeout elapsed.
    pub fn graceful_shutdown_with_timeout(self, timeout: std::time::Duration) -> bool {
        self.do_graceful_shutdown(Some(timeout))
    }

    fn do_graceful_shutdown(self, timeout: Option<std::time::Duration>) -> bool {
        let graceful_count = self.graceful_tasks.load();
        debug!(graceful_tasks = graceful_count, "firing shutdown signal");
        drop(self.signal);

        let completed = match timeout {
            Some(t) => self.graceful_tasks.wait_timeout(t),
            None => self.graceful_tasks.wait(),
        };

        if completed {
            debug!("gracefully shut down");
        } else {
            debug!("graceful shutdown timed out");
        }
        completed
    }
}

/// An endless future that resolves if a critical task panicked.
///
/// See [`TaskExecutor::spawn_critical`]
impl Future for TaskManager {
    type Output = Result<(), PanickedTaskError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match ready!(self.as_mut().get_mut().task_events_rx.poll_recv(cx)) {
            Some(TaskEvent::Panic(err)) => Poll::Ready(Err(err)),
            Some(TaskEvent::GracefulShutdown) | None => {
                if let Some(signal) = self.get_mut().signal.take() {
                    signal.fire();
                }
                Poll::Ready(Ok(()))
            }
        }
    }
}

/// Error with the name of the task that panicked and an error downcasted to string, if possible.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub struct PanickedTaskError {
    task_name: &'static str,
    error: Option<String>,
}

impl Display for PanickedTaskError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let task_name = self.task_name;
        if let Some(error) = &self.error {
            write!(f, "Critical task `{task_name}` panicked: `{error}`")
        } else {
            write!(f, "Critical task `{task_name}` panicked")
        }
    }
}

impl PanickedTaskError {
    fn new(task_name: &'static str, error: Box<dyn Any>) -> Self {
        let error = match error.downcast::<String>() {
            Ok(value) => Some(*value),
            Err(error) => match error.downcast::<&str>() {
                Ok(value) => Some(value.to_string()),
                Err(_) => None,
            },
        };

        Self { task_name, error }
    }
}

/// Represents the events that the `TaskManager`'s main future can receive.
#[derive(Debug)]
enum TaskEvent {
    /// Indicates that a critical task has panicked.
    Panic(PanickedTaskError),
    /// A signal requesting a graceful shutdown of the `TaskManager`.
    GracefulShutdown,
}

/// A type that can spawn new tokio tasks
#[derive(Debug, Clone)]
pub struct TaskExecutor {
    /// Handle to the tokio runtime this task manager is associated with.
    handle: Handle,
    /// Receiver of the shutdown signal.
    on_shutdown: Shutdown,
    /// Sender half for sending task events to this type
    task_events_tx: UnboundedSender<TaskEvent>,
    /// Task Executor Metrics
    metrics: TaskExecutorMetrics,
    /// How many [`GracefulShutdown`] tasks are currently active
    graceful_tasks: Arc<GracefulShutdownCounter>,
}

impl TaskExecutor {
    /// Attempts to get the current `TaskExecutor` if one has been initialized.
    ///
    /// Returns an error if no [`TaskExecutor`] has been initialized via [`TaskManager`].
    pub fn try_current() -> Result<Self, NoCurrentTaskExecutorError> {
        GLOBAL_EXECUTOR
            .get()
            .cloned()
            .ok_or_else(NoCurrentTaskExecutorError::default)
    }

    /// Returns the current `TaskExecutor`.
    ///
    /// # Panics
    ///
    /// Panics if no global executor has been initialized. Use [`try_current`](Self::try_current)
    /// for a non-panicking version.
    pub fn current() -> Self {
        Self::try_current().expect("TaskExecutor::current() called before TaskManager was created")
    }

    /// Returns the [Handle] to the tokio runtime.
    pub const fn handle(&self) -> &Handle {
        &self.handle
    }

    /// Returns the receiver of the shutdown signal.
    pub const fn on_shutdown_signal(&self) -> &Shutdown {
        &self.on_shutdown
    }

    /// Creates a new [`GracefulShutdown`] that participates in the graceful shutdown count.
    fn new_graceful_shutdown(&self) -> GracefulShutdown {
        GracefulShutdown::new(
            self.on_shutdown.clone(),
            GracefulShutdownGuard::new(Arc::clone(&self.graceful_tasks)),
        )
    }

    /// Spawns a future on the tokio runtime depending on the [`TaskKind`]
    fn spawn_on_rt<F>(&self, fut: F, task_kind: TaskKind) -> JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        match task_kind {
            TaskKind::Default => self.handle.spawn(fut),
            TaskKind::Blocking => {
                let handle = self.handle.clone();
                self.handle.spawn_blocking(move || handle.block_on(fut))
            }
        }
    }

    /// Single implementation for all regular (non-critical) tasks.
    ///
    /// Takes a pre-composed future. Callers handle shutdown composition before calling this.
    fn spawn_task_as<F>(
        &self,
        fut: F,
        task_kind: TaskKind,
        task_name: &'static str,
        graceful: bool,
    ) -> JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (finished_counter, running_gauge) = match task_kind {
            TaskKind::Default => {
                self.metrics.inc_regular_tasks(task_name);
                (
                    self.metrics.finished_regular_tasks_total(task_name),
                    self.metrics.running_task(task_name, "regular", graceful),
                )
            }
            TaskKind::Blocking => {
                self.metrics.inc_regular_blocking_tasks(task_name);
                (
                    self.metrics.finished_regular_blocking_tasks_total(task_name),
                    self.metrics.running_task(task_name, "blocking", graceful),
                )
            }
        };

        let task = async move {
            let _guard = OperationGuard::new(running_gauge, finished_counter);
            fut.await;
        };

        self.spawn_on_rt(task, task_kind)
    }

    /// Single implementation for all critical tasks.
    ///
    /// Takes a pre-composed future. Callers handle shutdown composition before calling this.
    fn spawn_critical_as<F>(
        &self,
        name: &'static str,
        fut: F,
        task_kind: TaskKind,
        graceful: bool,
    ) -> JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.metrics.inc_critical_tasks(name);
        let panicked_tasks_tx = self.task_events_tx.clone();
        let metrics = self.metrics;

        let running_gauge = metrics.running_task(name, "critical", graceful);
        let finished_counter = metrics.finished_critical_tasks_total(name);

        let task = std::panic::AssertUnwindSafe(fut)
            .catch_unwind()
            .map_err(move |error| {
                metrics.record_critical_panic();
                let task_error = PanickedTaskError::new(name, error);
                error!("{task_error}");
                if panicked_tasks_tx.send(TaskEvent::Panic(task_error)).is_err() {
                    warn!(task = name, "failed to notify TaskManager of panic (already shut down)");
                }
            })
            .map(drop);

        let task = async move {
            let _guard = OperationGuard::new(running_gauge, finished_counter);
            task.await;
        };

        self.spawn_on_rt(task, task_kind)
    }

    /// Spawns the task onto the runtime.
    /// The given future resolves as soon as the [Shutdown] signal is received.
    ///
    /// See also [`Handle::spawn`].
    pub fn spawn<F>(&self, fut: F) -> JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let on_shutdown = self.on_shutdown.clone();
        let fut = async move {
            let fut = pin!(fut);
            let _ = select(on_shutdown, fut).await;
        };
        self.spawn_task_as(fut, TaskKind::Default, "<unnamed>", false)
    }

    /// Spawns a blocking task onto the runtime.
    /// The given future resolves as soon as the [Shutdown] signal is received.
    ///
    /// See also [`Handle::spawn_blocking`].
    pub fn spawn_blocking<F>(&self, name: &'static str, fut: F) -> JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let on_shutdown = self.on_shutdown.clone();
        let fut = async move {
            let fut = pin!(fut);
            let _ = select(on_shutdown, fut).await;
        };
        self.spawn_task_as(fut, TaskKind::Blocking, name, false)
    }

    /// This spawns a critical task onto the runtime.
    /// The given future resolves as soon as the [Shutdown] signal is received.
    ///
    /// If this task panics, the [`TaskManager`] is notified.
    pub fn spawn_critical<F>(&self, name: &'static str, fut: F) -> JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        debug!(task = name, "spawning critical task");
        let on_shutdown = self.on_shutdown.clone();
        let fut = async move {
            let fut = pin!(fut);
            match select(on_shutdown, fut).await {
                Either::Left(_) => {
                    debug!(task = name, "critical task cancelled by shutdown signal");
                }
                Either::Right(_) => {
                    debug!(task = name, "critical task completed normally");
                }
            }
        };
        self.spawn_critical_as(name, fut, TaskKind::Default, false)
    }

    /// This spawns a critical blocking task onto the runtime.
    /// The given future resolves as soon as the [Shutdown] signal is received.
    ///
    /// If this task panics, the [`TaskManager`] is notified.
    pub fn spawn_critical_blocking<F>(&self, name: &'static str, fut: F) -> JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let on_shutdown = self.on_shutdown.clone();
        let fut = async move {
            let fut = pin!(fut);
            let _ = select(on_shutdown, fut).await;
        };
        self.spawn_critical_as(name, fut, TaskKind::Blocking, false)
    }

    /// Spawns a critical task that participates in graceful shutdown.
    ///
    /// If this task panics, the [`TaskManager`] is notified.
    /// The [`TaskManager`] will wait until the given future has completed before shutting down.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn t(executor: vertex_tasks::TaskExecutor) {
    ///
    /// executor.spawn_critical_with_graceful_shutdown_signal("grace", |shutdown| async move {
    ///     // await the shutdown signal
    ///     let guard = shutdown.await;
    ///     // do work before exiting the program
    ///     tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    ///     // allow graceful shutdown
    ///     drop(guard);
    /// });
    /// # }
    /// ```
    pub fn spawn_critical_with_graceful_shutdown_signal<F>(
        &self,
        name: &'static str,
        f: impl FnOnce(GracefulShutdown) -> F,
    ) -> JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        debug!(task = name, "spawning critical task with graceful shutdown");
        let fut = f(self.new_graceful_shutdown());
        self.spawn_critical_as(name, fut, TaskKind::Default, true)
    }

    /// Spawns a regular task that participates in graceful shutdown.
    ///
    /// The [`TaskManager`] will wait until the given future has completed before shutting down.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn t(executor: vertex_tasks::TaskExecutor) {
    ///
    /// executor.spawn_with_graceful_shutdown_signal("my_task", |shutdown| async move {
    ///     // await the shutdown signal
    ///     let guard = shutdown.await;
    ///     // do work before exiting the program
    ///     tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    ///     // allow graceful shutdown
    ///     drop(guard);
    /// });
    /// # }
    /// ```
    pub fn spawn_with_graceful_shutdown_signal<F>(
        &self,
        name: &'static str,
        f: impl FnOnce(GracefulShutdown) -> F,
    ) -> JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        debug!(task = name, "spawning task with graceful shutdown");
        let fut = f(self.new_graceful_shutdown());
        self.spawn_task_as(fut, TaskKind::Default, name, true)
    }

    /// Spawns a [`SpawnableTask`] as a critical task with graceful shutdown.
    ///
    /// This is the preferred way to spawn long-running services that implement
    /// [`SpawnableTask`]. The service receives a [`GracefulShutdown`] signal and
    /// is monitored for panics.
    pub fn spawn_service<S: SpawnableTask>(&self, name: &'static str, service: S) -> JoinHandle<()> {
        self.spawn_critical_with_graceful_shutdown_signal(name, |shutdown| {
            service.into_task(shutdown)
        })
    }

    /// Sends a request to the `TaskManager` to initiate a graceful shutdown.
    ///
    /// Caution: This will terminate the entire program.
    ///
    /// The [`TaskManager`] upon receiving this event, will terminate and initiate the shutdown that
    /// can be handled via the returned [`GracefulShutdown`].
    pub fn initiate_graceful_shutdown(
        &self,
    ) -> Result<GracefulShutdown, tokio::sync::mpsc::error::SendError<()>> {
        self.task_events_tx
            .send(TaskEvent::GracefulShutdown)
            .map_err(|_send_error_with_task_event| tokio::sync::mpsc::error::SendError(()))?;

        Ok(self.new_graceful_shutdown())
    }
}

impl TaskSpawner for TaskExecutor {
    fn spawn(&self, fut: BoxFuture<'static, ()>) -> JoinHandle<()> {
        Self::spawn(self, fut)
    }

    fn spawn_critical(&self, name: &'static str, fut: BoxFuture<'static, ()>) -> JoinHandle<()> {
        Self::spawn_critical(self, name, fut)
    }

    fn spawn_blocking(&self, name: &'static str, fut: BoxFuture<'static, ()>) -> JoinHandle<()> {
        Self::spawn_blocking(self, name, fut)
    }

    fn spawn_critical_blocking(
        &self,
        name: &'static str,
        fut: BoxFuture<'static, ()>,
    ) -> JoinHandle<()> {
        Self::spawn_critical_blocking(self, name, fut)
    }
}

/// Determines how a task is spawned
enum TaskKind {
    /// Spawn the task to the default executor [`Handle::spawn`]
    Default,
    /// Spawn the task to the blocking executor [`Handle::spawn_blocking`]
    Blocking,
}

/// Error returned by `try_current` when no task executor has been configured.
#[derive(Debug, Default, thiserror::Error)]
#[error("No current task executor available.")]
#[non_exhaustive]
pub struct NoCurrentTaskExecutorError;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::Duration,
    };

    #[test]
    fn test_cloneable() {
        #[derive(Clone)]
        struct ExecutorWrapper {
            _e: Box<dyn TaskSpawner>,
        }

        let executor: Box<dyn TaskSpawner> = Box::<TokioTaskExecutor>::default();
        let _e = dyn_clone::clone_box(&*executor);

        let e = ExecutorWrapper { _e };
        let _e2 = e;
    }

    #[test]
    fn test_critical() {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let handle = runtime.handle().clone();
        let manager = TaskManager::new(handle);
        let executor = manager.executor();

        executor.spawn_critical("this is a critical task", async {
            panic!("intentionally panic")
        });

        runtime.block_on(async move {
            let err_result = manager.await;
            assert!(
                err_result.is_err(),
                "Expected TaskManager to return an error due to panic"
            );
            let panicked_err = err_result.unwrap_err();

            assert_eq!(panicked_err.task_name, "this is a critical task");
            assert_eq!(panicked_err.error, Some("intentionally panic".to_string()));
        })
    }

    // Tests that spawned tasks are terminated if the `TaskManager` drops
    #[test]
    fn test_manager_shutdown_critical() {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let handle = runtime.handle().clone();
        let manager = TaskManager::new(handle.clone());
        let executor = manager.executor();

        let (signal, shutdown) = signal();

        executor.spawn_critical("this is a critical task", async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(signal);
        });

        drop(manager);

        handle.block_on(shutdown);
    }

    // Tests that spawned tasks are terminated if the `TaskManager` drops
    #[test]
    fn test_manager_shutdown() {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let handle = runtime.handle().clone();
        let manager = TaskManager::new(handle.clone());
        let executor = manager.executor();

        let (signal, shutdown) = signal();

        executor.spawn(Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(signal);
        }));

        drop(manager);

        handle.block_on(shutdown);
    }

    #[test]
    fn test_manager_graceful_shutdown() {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let handle = runtime.handle().clone();
        let manager = TaskManager::new(handle);
        let executor = manager.executor();

        let val = Arc::new(AtomicBool::new(false));
        let c = val.clone();
        executor.spawn_critical_with_graceful_shutdown_signal("grace", |shutdown| async move {
            let _guard = shutdown.await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            c.store(true, Ordering::Relaxed);
        });

        manager.graceful_shutdown();
        assert!(val.load(Ordering::Relaxed));
    }

    #[test]
    fn test_manager_graceful_shutdown_many() {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let handle = runtime.handle().clone();
        let manager = TaskManager::new(handle);
        let executor = manager.executor();

        let counter = Arc::new(AtomicUsize::new(0));
        let num = 10;
        for _ in 0..num {
            let c = counter.clone();
            executor.spawn_critical_with_graceful_shutdown_signal(
                "grace",
                move |shutdown| async move {
                    let _guard = shutdown.await;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    c.fetch_add(1, Ordering::SeqCst);
                },
            );
        }

        manager.graceful_shutdown();
        assert_eq!(counter.load(Ordering::Relaxed), num);
    }

    #[test]
    fn test_manager_graceful_shutdown_timeout() {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let handle = runtime.handle().clone();
        let manager = TaskManager::new(handle);
        let executor = manager.executor();

        let timeout = Duration::from_millis(500);
        let val = Arc::new(AtomicBool::new(false));
        let val2 = val.clone();
        executor.spawn_critical_with_graceful_shutdown_signal("grace", |shutdown| async move {
            let _guard = shutdown.await;
            tokio::time::sleep(timeout * 3).await;
            val2.store(true, Ordering::Relaxed);
        });

        manager.graceful_shutdown_with_timeout(timeout);
        assert!(!val.load(Ordering::Relaxed));
    }

    #[test]
    fn test_graceful_shutdown_triggered_by_executor() {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let task_manager = TaskManager::new(runtime.handle().clone());
        let executor = task_manager.executor();

        let task_did_shutdown_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = task_did_shutdown_flag.clone();

        let spawned_task_handle =
            executor.spawn_with_graceful_shutdown_signal("test_task", |shutdown| async move {
                let _guard = shutdown.await;
                flag_clone.store(true, Ordering::SeqCst);
            });

        let manager_future_handle = runtime.spawn(task_manager);

        let send_result = executor.initiate_graceful_shutdown();
        assert!(
            send_result.is_ok(),
            "Sending the graceful shutdown signal should succeed and return a GracefulShutdown future"
        );

        let manager_final_result = runtime.block_on(manager_future_handle);

        assert!(
            manager_final_result.is_ok(),
            "TaskManager task should not panic"
        );
        assert_eq!(
            manager_final_result.unwrap(),
            Ok(()),
            "TaskManager should resolve cleanly with Ok(()) after graceful shutdown request"
        );

        let task_join_result = runtime.block_on(spawned_task_handle);
        assert!(
            task_join_result.is_ok(),
            "Spawned task should complete without panic"
        );

        assert!(
            task_did_shutdown_flag.load(Ordering::Relaxed),
            "Task should have received the shutdown signal and set the flag"
        );
    }
}
