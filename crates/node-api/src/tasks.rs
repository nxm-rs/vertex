//! Task management for the Vertex Swarm node

use vertex_primitives::Result;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

/// A type that can spawn and manage async tasks
#[auto_impl::auto_impl(&, Arc)]
pub trait TaskExecutor: Clone + Send + Sync + 'static {
    /// Spawn a task
    fn spawn(&self, name: &'static str, task: BoxFuture<'static, ()>);

    /// Spawn a critical task that should crash the process on failure
    fn spawn_critical(&self, name: &'static str, task: BoxFuture<'static, ()>);

    /// Spawn a blocking task
    fn spawn_blocking(&self, name: &'static str, task: BoxBlockingFuture<'static, ()>);

    /// Spawn a task with a shutdown signal
    fn spawn_with_shutdown(
        &self,
        name: &'static str,
        task: BoxShutdownFuture<'static, ()>,
    );

    /// Shutdown all tasks and wait for them to complete
    fn shutdown(self) -> BoxFuture<'static, ()>;
}

/// A boxed future
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A boxed blocking future
pub type BoxBlockingFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A boxed future that can be shut down
pub type BoxShutdownFuture<'a, T> = Pin<Box<dyn ShutdownFuture<Output = T> + Send + 'a>>;

/// A future that can be shut down
pub trait ShutdownFuture: Future {
    /// Signal the future to shut down
    fn shutdown(&self);
}

/// A task manager that creates and manages a tokio runtime
pub struct TaskManager {
    /// Handle to the runtime
    runtime: tokio::runtime::Handle,
    /// Shutdown signal
    shutdown_tx: tokio::sync::mpsc::Sender<()>,
    /// Shutdown wait channel
    shutdown_complete_rx: tokio::sync::oneshot::Receiver<()>,
}

impl TaskManager {
    /// Create a new task manager with a new tokio runtime
    #[cfg(feature = "std")]
    pub fn new() -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| vertex_primitives::Error::Other(format!("Failed to create runtime: {}", e)))?;

        Self::with_runtime(runtime.handle().clone())
    }

    /// Create a new task manager with an existing tokio runtime
    pub fn with_runtime(runtime: tokio::runtime::Handle) -> Result<Self> {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);
        let (shutdown_complete_tx, shutdown_complete_rx) = tokio::sync::oneshot::channel();

        // Spawn shutdown task
        runtime.spawn(async move {
            let _ = shutdown_rx.recv().await;
            let _ = shutdown_complete_tx.send(());
        });

        Ok(Self {
            runtime,
            shutdown_tx,
            shutdown_complete_rx,
        })
    }

    /// Returns a new executor that can spawn tasks on this runtime
    pub fn executor(&self) -> TaskExecutorImpl {
        TaskExecutorImpl {
            runtime: self.runtime.clone(),
            shutdown_tx: self.shutdown_tx.clone(),
        }
    }

    /// Wait for all tasks to complete
    pub async fn wait_for_shutdown(self) -> Result<()> {
        let _ = self.shutdown_tx.send(()).await;
        self.shutdown_complete_rx.await
            .map_err(|e| vertex_primitives::Error::Other(format!("Failed to wait for shutdown: {}", e)))
    }
}

/// Implementation of TaskExecutor using tokio
#[derive(Clone)]
pub struct TaskExecutorImpl {
    /// Handle to the runtime
    runtime: tokio::runtime::Handle,
    /// Shutdown signal
    shutdown_tx: tokio::sync::mpsc::Sender<()>,
}

impl TaskExecutor for TaskExecutorImpl {
    fn spawn(&self, name: &'static str, task: BoxFuture<'static, ()>) {
        self.runtime.spawn(async move {
            if let Err(e) = task.await {
                tracing::error!("Task {} failed: {:?}", name, e);
            }
        });
    }

    fn spawn_critical(&self, name: &'static str, task: BoxFuture<'static, ()>) {
        self.runtime.spawn(async move {
            if let Err(e) = task.await {
                tracing::error!("Critical task {} failed: {:?}", name, e);
                std::process::exit(1);
            }
        });
    }

    fn spawn_blocking(&self, name: &'static str, task: BoxBlockingFuture<'static, ()>) {
        self.runtime.spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create runtime");

            if let Err(e) = rt.block_on(task) {
                tracing::error!("Blocking task {} failed: {:?}", name, e);
            }
        });
    }

    fn spawn_with_shutdown(
        &self,
        name: &'static str,
        mut task: BoxShutdownFuture<'static, ()>,
    ) {
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        self.runtime.spawn(async move {
            tokio::select! {
                _ = &mut task => {},
                _ = shutdown_rx.recv() => {
                    task.shutdown();
                }
            }
        });
    }

    fn shutdown(self) -> BoxFuture<'static, ()> {
        let shutdown_tx = self.shutdown_tx;
        Box::pin(async move {
            let _ = shutdown_tx.send(()).await;
        })
    }
}

/// A simple wrapper around a future that can be shut down
pub struct ShutdownWrapper<F> {
    /// The inner future
    inner: F,
    /// Whether the future should shut down
    should_shutdown: std::sync::atomic::AtomicBool,
}

impl<F> ShutdownWrapper<F> {
    /// Create a new shutdown wrapper
    pub fn new(inner: F) -> Self {
        Self {
            inner,
            should_shutdown: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl<F: Future> Future for ShutdownWrapper<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: We're not moving the future inside the pin
        let this = unsafe { self.get_unchecked_mut() };

        if this.should_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            return Poll::Ready(unsafe {
                // This is safe because we're in the Poll::Ready branch,
                // which means we're not going to poll this future again
                std::mem::uninitialized()
            });
        }

        // Safety: We're not moving the future
        unsafe { Pin::new_unchecked(&mut this.inner) }.poll(cx)
    }
}

impl<F> ShutdownFuture for ShutdownWrapper<F>
where
    F: Future,
{
    fn shutdown(&self) {
        self.should_shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
