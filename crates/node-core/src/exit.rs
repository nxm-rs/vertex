//! Helper types for waiting for the node to exit.

use futures::FutureExt;
use std::{
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};
use tokio::sync::oneshot;

/// A Future which resolves when the node exits
#[derive(Debug)]
pub struct NodeExitFuture {
    /// The receiver half of a channel. This can be used to wait for
    /// some task at the other end of the channel to complete.
    wait_task_rx: Option<oneshot::Receiver<Result<(), eyre::Error>>>,

    /// Flag indicating whether the node should be terminated afer workload intensive task.
    terminate: bool,
}

impl NodeExitFuture {
    /// Create a new `NodeExitFuture`.
    pub const fn new(
        wait_task_rx: oneshot::Receiver<Result<(), eyre::Error>>,
        terminate: bool,
    ) -> Self {
        Self { wait_task_rx: Some(wait_task_rx), terminate }
    }
}

impl Future for NodeExitFuture {
    type Output = eyre::Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(rx) = this.wait_task_rx.as_mut() {
            match ready!(rx.poll_unpin(cx)) {
                Ok(res) => {
                    this.wait_task_rx.take();
                    res?;
                    if this.terminate {
                        Poll::Ready(Ok(()))
                    } else {
                        Poll::Pending
                    }
                }
                Err(err) => Poll::Ready(Err(err.into())),
            }
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::poll_fn;

    #[tokio::test]
    async fn test_node_exit_future_terminate_true() {
        let (tx, rx) = oneshot::channel::<Result<(), eyre::Error>>();

        let _ = tx.send(Ok(()));

        let node_exit_future = NodeExitFuture::new(rx, true);

        let res = node_exit_future.await;

        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_node_exit_future_terminate_false() {
        let (tx, rx) = oneshot::channel::<Result<(), eyre::Error>>();

        let _ = tx.send(Ok(()));

        let mut node_exit_future = NodeExitFuture::new(rx, false);
        poll_fn(|cx| {
            assert!(node_exit_future.poll_unpin(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
}
