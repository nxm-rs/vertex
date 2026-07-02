//! The serve-or-delegate driver for inbound retrieval and pushsync.
//!
//! Both inbound protocols share one shape: fulfil locally (a cache hit, or
//! taking custody), else delegate to the [`Forwarder`]; either way the answer's
//! un-applied provide commits only after the wire write and is forfeited when
//! the peer refuses delivery, while every failure on our side releases without
//! a trace. [`drive`] owns that shape once; a [`ServeOp`] carries the
//! operation-specific fulfilment, delegation, payload and outcome vocabulary.

use std::fmt::Display;
use std::future::Future;
use std::sync::Arc;

use nectar_primitives::{AnyChunk, ChunkAddress};
use tracing::debug;
use vertex_swarm_api::{CommitOnWrite, SwarmLocalStore};
use vertex_swarm_net_pushsync::{PushsyncError, PushsyncResponder, Receipt, WireReceipt};
use vertex_swarm_net_retrieval::{RetrievalError, RetrievalResponder};
use vertex_swarm_primitives::{CachedChunk, OverlayAddress, Stamp, StampedChunk};

use super::forward::{ForwardError, Forwarder};
use super::handler::InboundOutcome;
use super::storer::StorerCapability;

/// An answer in hand together with its un-applied upstream credit.
pub(crate) struct Fulfilment<P> {
    pub payload: P,
    pub provide: Box<dyn CommitOnWrite>,
}

/// The local-fulfilment verdict.
pub(crate) enum Local<P> {
    /// Answer in hand, billed; respond and commit.
    Fulfilled(Fulfilment<P>),
    /// Not ours to answer; delegate to the forwarder.
    Delegate,
    /// Cannot answer (gate refusal, custody failure); reset without delegating.
    Refuse,
}

/// One inbound serve operation: local fulfilment, delegation, the wire write
/// and the outcome vocabulary. [`drive`] sequences them.
pub(crate) trait ServeOp: Send + Sync + Sized + 'static {
    /// The payload written back to the peer.
    type Payload: Send;
    /// The consumed wire responder.
    type Responder: Send;
    /// The wire write error, logged on a refused delivery.
    type SendError: Display;

    /// Try to fulfil from what this node already holds. A `Fulfilled` answer
    /// carries its billed provide; `Refuse` means the request must reset
    /// without delegating.
    fn local(&self) -> impl Future<Output = Local<Self::Payload>> + Send;

    /// Delegate to the forwarder. A provide released for a failure on our
    /// side must be dropped (not forfeited) before returning the error.
    fn delegate(
        &self,
    ) -> impl Future<Output = Result<Fulfilment<Self::Payload>, ForwardError>> + Send;

    /// Write the payload back to the peer, consuming the responder.
    fn respond(
        responder: Self::Responder,
        payload: Self::Payload,
    ) -> impl Future<Output = Result<(), Self::SendError>> + Send;

    /// Reset the substream without a payload.
    fn refuse(responder: Self::Responder);

    /// The requesting peer, for the driver's delivery-refused log.
    fn peer(&self) -> OverlayAddress;

    /// The chunk address, for the driver's delivery-refused log.
    fn address(&self) -> ChunkAddress;

    /// Outcome when the locally fulfilled answer was delivered.
    fn fulfilled(&self) -> InboundOutcome;

    /// Outcome when the delegated answer was delivered.
    fn delegated(&self) -> InboundOutcome;

    /// Outcome when nothing was delivered; the substream was reset.
    fn failed(&self) -> InboundOutcome;
}

/// Serve one inbound request: local fulfilment or delegation, then the shared
/// respond-and-commit tail.
pub(crate) async fn drive<Op: ServeOp>(op: Op, responder: Op::Responder) -> InboundOutcome {
    match op.local().await {
        Local::Fulfilled(fulfilment) => {
            let success = op.fulfilled();
            respond_and_commit(&op, responder, fulfilment, success).await
        }
        Local::Refuse => {
            Op::refuse(responder);
            op.failed()
        }
        Local::Delegate => match op.delegate().await {
            Ok(fulfilment) => {
                let success = op.delegated();
                respond_and_commit(&op, responder, fulfilment, success).await
            }
            Err(_) => {
                Op::refuse(responder);
                op.failed()
            }
        },
    }
}

/// Write the answer back and settle its provide: commit on a landed write,
/// forfeit when the peer refused delivery of an answer in hand (the ghost
/// trace that starves repeat refusers).
async fn respond_and_commit<Op: ServeOp>(
    op: &Op,
    responder: Op::Responder,
    fulfilment: Fulfilment<Op::Payload>,
    success: InboundOutcome,
) -> InboundOutcome {
    match Op::respond(responder, fulfilment.payload).await {
        Ok(()) => {
            fulfilment.provide.apply_boxed();
            success
        }
        Err(e) => {
            debug!(
                peer = %op.peer(),
                address = %op.address(),
                error = %e,
                "serve delivery refused by the peer"
            );
            fulfilment.provide.forfeit_boxed();
            op.failed()
        }
    }
}

/// Inbound retrieval: cache hit (content indefinitely, single-owner while
/// fresh), else forward to a closer peer.
pub(crate) struct RetrieveServe {
    pub store: Arc<dyn SwarmLocalStore>,
    pub forward: Arc<dyn Forwarder>,
    pub overlay: OverlayAddress,
    pub address: ChunkAddress,
}

impl ServeOp for RetrieveServe {
    type Payload = (AnyChunk, Option<Stamp>);
    type Responder = RetrievalResponder;
    type SendError = RetrievalError;

    async fn local(&self) -> Local<Self::Payload> {
        // Cache hit: the store applies the single-owner TTL on `get`. Serve
        // whichever stamp the cache held. A terminal serve is billed like a
        // relay: reserve the upstream credit before responding; a refusal
        // means the requester is past its settle line, so the serve is
        // refused too.
        let Ok(Some(cached)) = self.store.get(&self.address) else {
            return Local::Delegate;
        };
        if *cached.address() != self.address {
            return Local::Delegate;
        }
        match self.forward.prepare_serve(self.overlay, &self.address) {
            Ok(provide) => {
                let (chunk, stamp) = cached.into_parts();
                Local::Fulfilled(Fulfilment {
                    payload: (chunk, stamp),
                    provide,
                })
            }
            Err(_) => Local::Refuse,
        }
    }

    async fn delegate(&self) -> Result<Fulfilment<Self::Payload>, ForwardError> {
        let forwarded = self.forward.retrieve(self.address, self.overlay).await?;
        if *forwarded.chunk.address() != self.address {
            // Wrong address means a relay bug, not the requester's fault;
            // release the credit without a trace and reset.
            drop(forwarded.provide);
            return Err(ForwardError::UnverifiedRelay);
        }
        // Only content chunks are cached (immutable, address-keyed); a
        // retrieved SOC has no version signal so it is relayed but never
        // stored.
        if forwarded.chunk.is_content() {
            let _ = self.store.put(CachedChunk::new(
                forwarded.chunk.clone(),
                forwarded.stamp.clone(),
            ));
        }
        Ok(Fulfilment {
            payload: (forwarded.chunk, forwarded.stamp),
            provide: forwarded.provide,
        })
    }

    async fn respond(
        responder: RetrievalResponder,
        (chunk, stamp): Self::Payload,
    ) -> Result<(), RetrievalError> {
        responder.send_chunk(chunk, stamp).await
    }

    fn refuse(responder: RetrievalResponder) {
        responder.send_error();
    }

    fn peer(&self) -> OverlayAddress {
        self.overlay
    }

    fn address(&self) -> ChunkAddress {
        self.address
    }

    fn fulfilled(&self) -> InboundOutcome {
        InboundOutcome::Served {
            overlay: self.overlay,
        }
    }

    fn delegated(&self) -> InboundOutcome {
        InboundOutcome::Forwarded {
            overlay: self.overlay,
        }
    }

    fn failed(&self) -> InboundOutcome {
        InboundOutcome::Missed {
            overlay: self.overlay,
            address: self.address,
        }
    }
}

/// Inbound pushsync: take custody when responsible (store, sign, acknowledge),
/// else forward to a closer peer and relay the storer's receipt verbatim.
pub(crate) struct PushServe {
    pub storer: Option<StorerCapability>,
    pub forward: Arc<dyn Forwarder>,
    pub overlay: OverlayAddress,
    pub chunk: StampedChunk,
}

impl ServeOp for PushServe {
    type Payload = WireReceipt;
    type Responder = PushsyncResponder;
    type SendError = PushsyncError;

    async fn local(&self) -> Local<WireReceipt> {
        let address = *self.chunk.address();
        // Storer ingest: only when responsible for the chunk. Absent on a
        // client.
        let Some(storer) = self
            .storer
            .as_ref()
            .filter(|storer| storer.reserve.is_responsible_for(&address))
        else {
            return Local::Delegate;
        };

        // Custody is billed like any serve: reserve the upstream credit
        // before the storage work; a gate refusal refuses custody.
        let provide = match self.forward.prepare_serve(self.overlay, &address) {
            Ok(provide) => provide,
            Err(_) => return Local::Refuse,
        };

        // Persist before acknowledging: a receipt must never claim custody of
        // a chunk that is not durably in the reserve. Both failure arms below
        // drop `provide`, releasing without a trace: they are our failures,
        // not the pusher's.
        if let Err(e) = storer.reserve.put(CachedChunk::from(self.chunk.clone())) {
            debug!(peer = %self.overlay, %address, error = %e, "Reserve put failed; not acknowledging");
            return Local::Refuse;
        }

        // Sign our own custody receipt over the address, declaring our
        // current storage radius; an upstream forwarder recovers our overlay
        // from the signature.
        let storage_radius = storer.reserve.storage_radius();
        match Receipt::sign(&storer.signer, address, storage_radius) {
            Ok(receipt) => Local::Fulfilled(Fulfilment {
                payload: receipt.to_wire(),
                provide,
            }),
            Err(e) => {
                // Stored, but cannot prove custody. Reset rather than send an
                // unsigned ack; the pusher retries (the reserve put is
                // content-addressed, so a re-delivery is a no-op).
                debug!(peer = %self.overlay, %address, error = %e, "Receipt sign failed; not acknowledging");
                Local::Refuse
            }
        }
    }

    async fn delegate(&self) -> Result<Fulfilment<WireReceipt>, ForwardError> {
        let forwarded = self.forward.push(self.chunk.clone(), self.overlay).await?;
        // Relay the storer's receipt verbatim: we never sign. The signer was
        // verified at decode, so the wire bytes reproduce the storer's
        // signature, nonce, and radius unchanged.
        Ok(Fulfilment {
            payload: forwarded.receipt.to_wire(),
            provide: forwarded.provide,
        })
    }

    async fn respond(
        responder: PushsyncResponder,
        receipt: WireReceipt,
    ) -> Result<(), PushsyncError> {
        responder.send_receipt(receipt).await
    }

    fn refuse(responder: PushsyncResponder) {
        responder.send_error();
    }

    fn peer(&self) -> OverlayAddress {
        self.overlay
    }

    fn address(&self) -> ChunkAddress {
        *self.chunk.address()
    }

    fn fulfilled(&self) -> InboundOutcome {
        InboundOutcome::Stored {
            overlay: self.overlay,
        }
    }

    fn delegated(&self) -> InboundOutcome {
        InboundOutcome::Relayed {
            overlay: self.overlay,
        }
    }

    fn failed(&self) -> InboundOutcome {
        InboundOutcome::PushFailed {
            overlay: self.overlay,
            address: *self.chunk.address(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// Records whether the provide was committed or forfeited.
    #[derive(Default)]
    struct RecordingCommit {
        applied: Arc<AtomicBool>,
        forfeited: Arc<AtomicBool>,
    }

    impl CommitOnWrite for RecordingCommit {
        fn apply_boxed(self: Box<Self>) {
            self.applied.store(true, Ordering::SeqCst);
        }

        fn forfeit_boxed(self: Box<Self>) {
            self.forfeited.store(true, Ordering::SeqCst);
        }
    }

    /// Records refusal and scripts the send result; the payload write lands in
    /// `sent`.
    #[derive(Default)]
    struct TestResponder {
        refused: Arc<AtomicBool>,
        send_ok: bool,
        sent: Arc<Mutex<Vec<&'static str>>>,
    }

    impl TestResponder {
        fn sending(send_ok: bool) -> Self {
            Self {
                send_ok,
                ..Self::default()
            }
        }
    }

    /// A scriptable op: local verdict and delegation result.
    struct TestOp {
        local: Mutex<Option<Local<&'static str>>>,
        delegate: Mutex<Option<Result<Fulfilment<&'static str>, ForwardError>>>,
    }

    impl TestOp {
        fn new(
            local: Local<&'static str>,
            delegate: Result<Fulfilment<&'static str>, ForwardError>,
        ) -> Self {
            Self {
                local: Mutex::new(Some(local)),
                delegate: Mutex::new(Some(delegate)),
            }
        }
    }

    impl ServeOp for TestOp {
        type Payload = &'static str;
        type Responder = TestResponder;
        type SendError = &'static str;

        async fn local(&self) -> Local<&'static str> {
            self.local
                .lock()
                .unwrap()
                .take()
                .expect("local called once")
        }

        async fn delegate(&self) -> Result<Fulfilment<&'static str>, ForwardError> {
            self.delegate
                .lock()
                .unwrap()
                .take()
                .expect("delegate called once")
        }

        async fn respond(
            responder: TestResponder,
            payload: &'static str,
        ) -> Result<(), &'static str> {
            if responder.send_ok {
                responder.sent.lock().unwrap().push(payload);
                Ok(())
            } else {
                Err("peer reset the substream")
            }
        }

        fn refuse(responder: TestResponder) {
            responder.refused.store(true, Ordering::SeqCst);
        }

        fn peer(&self) -> OverlayAddress {
            OverlayAddress::from([0xaa; 32])
        }

        fn address(&self) -> ChunkAddress {
            ChunkAddress::from([0xbb; 32])
        }

        fn fulfilled(&self) -> InboundOutcome {
            InboundOutcome::Served {
                overlay: self.peer(),
            }
        }

        fn delegated(&self) -> InboundOutcome {
            InboundOutcome::Forwarded {
                overlay: self.peer(),
            }
        }

        fn failed(&self) -> InboundOutcome {
            InboundOutcome::Missed {
                overlay: self.peer(),
                address: self.address(),
            }
        }
    }

    fn fulfilment(
        payload: &'static str,
    ) -> (Fulfilment<&'static str>, Arc<AtomicBool>, Arc<AtomicBool>) {
        let commit = RecordingCommit::default();
        let applied = Arc::clone(&commit.applied);
        let forfeited = Arc::clone(&commit.forfeited);
        (
            Fulfilment {
                payload,
                provide: Box::new(commit),
            },
            applied,
            forfeited,
        )
    }

    #[tokio::test]
    async fn local_fulfilment_commits_on_a_landed_write() {
        let (fulfilment, applied, forfeited) = fulfilment("cached");
        let op = TestOp::new(
            Local::Fulfilled(fulfilment),
            Err(ForwardError::NoCloserPeer),
        );
        let responder = TestResponder::sending(true);
        let sent = Arc::clone(&responder.sent);

        let outcome = drive(op, responder).await;

        assert!(matches!(outcome, InboundOutcome::Served { .. }));
        assert_eq!(*sent.lock().unwrap(), vec!["cached"]);
        assert!(applied.load(Ordering::SeqCst));
        assert!(!forfeited.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn refused_delivery_forfeits_the_provide() {
        let (fulfilment, applied, forfeited) = fulfilment("cached");
        let op = TestOp::new(
            Local::Fulfilled(fulfilment),
            Err(ForwardError::NoCloserPeer),
        );

        let outcome = drive(op, TestResponder::sending(false)).await;

        assert!(matches!(outcome, InboundOutcome::Missed { .. }));
        assert!(!applied.load(Ordering::SeqCst));
        assert!(forfeited.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn local_refusal_resets_without_delegating() {
        let op = TestOp::new(Local::Refuse, Err(ForwardError::NoCloserPeer));
        let responder = TestResponder::sending(true);
        let refused = Arc::clone(&responder.refused);

        let outcome = drive(op, responder).await;

        assert!(matches!(outcome, InboundOutcome::Missed { .. }));
        assert!(refused.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn delegation_commits_on_a_landed_write() {
        let (fulfilment, applied, _) = fulfilment("relayed");
        let op = TestOp::new(Local::Delegate, Ok(fulfilment));
        let responder = TestResponder::sending(true);
        let sent = Arc::clone(&responder.sent);

        let outcome = drive(op, responder).await;

        assert!(matches!(outcome, InboundOutcome::Forwarded { .. }));
        assert_eq!(*sent.lock().unwrap(), vec!["relayed"]);
        assert!(applied.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn failed_delegation_resets() {
        let op = TestOp::new(Local::Delegate, Err(ForwardError::AllPeersFailed));
        let responder = TestResponder::sending(true);
        let refused = Arc::clone(&responder.refused);

        let outcome = drive(op, responder).await;

        assert!(matches!(outcome, InboundOutcome::Missed { .. }));
        assert!(refused.load(Ordering::SeqCst));
    }
}
