//! Node exit handling

use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use vertex_primitives::Result;

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;

/// A trait for node exit managers
#[auto_impl::auto_impl(&, Arc)]
pub trait ExitManager: Send + Sync + 'static {
    /// Register a handler to be called when the node is shutting down
    fn register_handler<F>(&self, handler: F)
    where
        F: FnOnce() + Send + 'static;

    /// Signal the node to exit
    fn signal_exit(&self);

    /// Returns true if the node should exit
    fn is_exiting(&self) -> bool;

    /// Wait for the node to complete the exit process
    fn wait_for_exit(&self) -> ExitFuture;
}

/// A future that resolves when the node has exited
pub struct ExitFuture {
    /// Inner future
    inner: Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>,
}

impl ExitFuture {
    /// Create a new exit future
    pub fn new<F>(future: F) -> Self
    where
        F: Future<Output = Result<()>> + Send + 'static,
    {
        Self { inner: Box::pin(future) }
    }
}

impl Future for ExitFuture {
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.inner.as_mut().poll(cx)
    }
}

/// Implementation of an exit manager using tokio channels
#[derive(Debug, Clone)]
pub struct TokioExitManager {
    /// Exit sender channel
    exit_tx: tokio::sync::broadcast::Sender<()>,
    /// Exit receiver channel
    exit_rx: tokio::sync::broadcast::Receiver<()>,
    /// Handlers to call on exit
    handlers: tokio::sync::Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
    /// Exit completion channel
    exit_complete_tx: tokio::sync::watch::Sender<bool>,
    /// Exit completion receiver
    exit_complete_rx: tokio::sync::watch::Receiver<bool>,
}

impl TokioExitManager {
    /// Create a new exit manager
    pub fn new() -> Self {
        let (exit_tx, exit_rx) = tokio::sync::broadcast::channel(1);
        let (exit_complete_tx, exit_complete_rx) = tokio::sync::watch::channel(false);

        Self {
            exit_tx,
            exit_rx,
            handlers: tokio::sync::Mutex::new(Vec::new()),
            exit_complete_tx,
            exit_complete_rx,
        }
    }

    /// Spawn a task that handles the exit process
    pub fn spawn_exit_handler(&self, task_executor: &crate::tasks::TaskExecutor) {
        let mut exit_rx = self.exit_tx.subscribe();
        let handlers = self.handlers.clone();
        let exit_complete_tx = self.exit_complete_tx.clone();

        task_executor.spawn("exit-handler", Box::pin(async move {
            let _ = exit_rx.recv().await;

            // Call all exit handlers
            let mut handlers = handlers.lock().await;
            for handler in handlers.drain(..) {
                handler();
            }

            // Signal exit completion
            let _ = exit_complete_tx.send(true);
        }));
    }
}

impl Default for TokioExitManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ExitManager for TokioExitManager {
    fn register_handler<F>(&self, handler: F)
    where
        F: FnOnce() + Send + 'static,
    {
        tokio::spawn(async move {
            let mut handlers = self.handlers.lock().await;
            handlers.push(Box::new(handler));
        });
    }

    fn signal_exit(&self) {
        let _ = self.exit_tx.send(());
    }

    fn is_exiting(&self) -> bool {
        *self.exit_complete_rx.borrow()
    }

    fn wait_for_exit(&self) -> ExitFuture {
        let mut exit_complete_rx = self.exit_complete_rx.clone();

        ExitFuture::new(async move {
            loop {
                if *exit_complete_rx.borrow() {
                    break;
                }

                exit_complete_rx.changed().await
                    .map_err(|e| vertex_primitives::Error::Other(format!("Exit watch error: {}", e)))?;
            }

            Ok(())
        })
    }
}
