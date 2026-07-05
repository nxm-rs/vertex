//! Vertex harness over `libp2p-swarm-test`.
//!
//! Behaviour tests re-roll the same plumbing: an ephemeral swarm, a memory
//! listener made external, a connect, then a hand-marked active peer in the
//! connection registry the behaviour reads through. This module folds that into
//! seeded identities, a first-class [`connect_and_activate`], and `start_paused`
//! aware drive helpers.
//!
//! Transport is an implementation detail: nodes run on the in-process memory
//! transport, but no test-facing signature names it, so a later deterministic
//! simulation can slot under the same surface.

use std::future::poll_fn;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{ConnectionId, NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, Swarm};
use libp2p_swarm_test::SwarmExt;
use tokio::sync::{mpsc, oneshot};
use vertex_net_peer_registry::PeerRegistry;
use vertex_swarm_api::SwarmNodeType;
use vertex_swarm_primitives::OverlayAddress;

use crate::MockIdentity;
use crate::peer::{test_keypair, test_overlay};

/// Connection registry a seeded swarm's behaviour reads through, keyed by
/// overlay. Topology is the writer in production; in a standalone harness swarm
/// the harness is, so [`connect_and_activate`] populates it directly.
pub type IdentityRegistry = Arc<PeerRegistry<OverlayAddress, ()>>;

/// A reproducible libp2p keypair paired with an overlay, both derived from one
/// `seed` byte.
///
/// Same seed, same `keypair`, `peer_id`, and `overlay` across runs, so a test
/// names a seed rather than carrying literal identities.
#[derive(Clone)]
pub struct SeededIdentity {
    /// Seed byte the identity was derived from.
    pub seed: u8,
    /// Deterministic libp2p keypair (the swarm identity).
    pub keypair: libp2p::identity::Keypair,
    /// PeerId of [`keypair`](Self::keypair).
    pub peer_id: PeerId,
    /// Deterministic overlay address (all bytes equal to `seed`).
    pub overlay: OverlayAddress,
    /// Node type the identity presents as.
    pub node_type: SwarmNodeType,
}

impl std::fmt::Debug for SeededIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeededIdentity")
            .field("seed", &self.seed)
            .field("peer_id", &self.peer_id)
            .field("overlay", &self.overlay)
            .field("node_type", &self.node_type)
            .finish()
    }
}

impl SeededIdentity {
    /// A [`MockIdentity`] carrying this overlay and node type (random signer).
    pub fn mock_identity(&self) -> MockIdentity {
        MockIdentity::with_overlay(self.overlay).with_node_type(self.node_type)
    }
}

/// Build the reproducible identity for `seed` presenting as `node_type`.
pub fn seeded_identity(seed: u8, node_type: SwarmNodeType) -> SeededIdentity {
    let keypair = test_keypair(seed);
    let peer_id = keypair.public().to_peer_id();
    SeededIdentity {
        seed,
        keypair,
        peer_id,
        overlay: test_overlay(seed),
        node_type,
    }
}

/// Values handed to the behaviour builder closure in [`seeded_node`].
pub struct NodeContext {
    /// The swarm's libp2p keypair.
    pub keypair: libp2p::identity::Keypair,
    /// The connection registry the behaviour should read active peers from.
    pub identities: IdentityRegistry,
    /// The node's overlay address.
    pub overlay: OverlayAddress,
    /// The node's node type.
    pub node_type: SwarmNodeType,
}

/// One seeded swarm plus the bookkeeping [`connect_and_activate`] needs.
///
/// `swarm` is public so callers issue behaviour commands and inspect state
/// directly; the drive helpers borrow it.
pub struct HarnessNode<B: NetworkBehaviour> {
    /// The libp2p swarm.
    pub swarm: Swarm<B>,
    /// The swarm's PeerId.
    pub peer_id: PeerId,
    /// The node's overlay address.
    pub overlay: OverlayAddress,
    /// The node's node type.
    pub node_type: SwarmNodeType,
    /// The registry the behaviour reads active peers from.
    pub identities: IdentityRegistry,
}

impl<B: NetworkBehaviour + Send> HarnessNode<B>
where
    B::ToSwarm: std::fmt::Debug,
{
    /// Build a memory-transport swarm for `id`, constructing the behaviour from
    /// a [`NodeContext`] so it can wire in the harness-owned identity registry.
    pub fn new(id: &SeededIdentity, build: impl FnOnce(&NodeContext) -> B) -> Self {
        let identities: IdentityRegistry = Arc::new(PeerRegistry::new());
        let overlay = id.overlay;
        let node_type = id.node_type;
        let registry = Arc::clone(&identities);
        let swarm =
            Swarm::new_ephemeral_memory_tokio_with_keypair(id.keypair.clone(), move |keypair| {
                build(&NodeContext {
                    keypair,
                    identities: registry,
                    overlay,
                    node_type,
                })
            });
        let peer_id = *swarm.local_peer_id();
        Self {
            swarm,
            peer_id,
            overlay,
            node_type,
            identities,
        }
    }
}

/// Build a seeded harness node in one call.
///
/// Shorthand for [`seeded_identity`] followed by [`HarnessNode::new`].
pub fn seeded_node<B: NetworkBehaviour + Send>(
    seed: u8,
    node_type: SwarmNodeType,
    build: impl FnOnce(&NodeContext) -> B,
) -> HarnessNode<B>
where
    B::ToSwarm: std::fmt::Debug,
{
    HarnessNode::new(&seeded_identity(seed, node_type), build)
}

/// Connection ids handed to the registry must be distinct across activations so
/// its per-connection indices stay unique. Seeded far above libp2p's own
/// allocator (which counts up from 1 in the same process) so a synthetic id can
/// never collide with a live connection's.
fn next_connection_id() -> ConnectionId {
    static N: AtomicUsize = AtomicUsize::new(usize::MAX >> 1);
    ConnectionId::new_unchecked(N.fetch_add(1, Ordering::Relaxed))
}

/// Mark `peer`/`overlay` active in `registry`, the transition topology performs
/// at handshake completion (an inbound connection promoted to a known overlay).
///
/// The entry is recorded as inbound under a synthetic [`ConnectionId`] that
/// never matches the live connection, so tests must not read direction or
/// per-connection identity back out of the harness registry.
fn activate_in_registry(registry: &IdentityRegistry, peer: PeerId, overlay: OverlayAddress) {
    let conn = next_connection_id();
    registry.connected_inbound(peer, conn);
    registry.activate(peer, conn, overlay);
}

/// Listen on the memory transport and advertise the address as external so a
/// peer can dial it, returning the bound multiaddr.
async fn listen_memory<B: NetworkBehaviour + Send>(swarm: &mut Swarm<B>) -> Multiaddr
where
    B::ToSwarm: std::fmt::Debug,
{
    let listener = swarm
        .listen_on(Protocol::Memory(0).into())
        .expect("memory transport accepts a /memory listen");
    let addr = loop {
        if let SwarmEvent::NewListenAddr {
            listener_id,
            address,
        } = swarm.select_next_some().await
            && listener_id == listener
        {
            break address;
        }
    };
    swarm.add_external_address(addr.clone());
    addr
}

/// Connect two seeded nodes and mark each peer active on the other side.
///
/// Listens both nodes on the memory transport, dials one to the other, writes
/// the active mapping into each node's registry (as topology would), then runs
/// `activate` on each behaviour so one gated on activation goes live. `activate`
/// receives the behaviour and the remote peer's identity; pass a no-op for a
/// behaviour with no activation step.
///
/// Both registry entries are recorded as inbound under synthetic connection ids
/// that never match the live connection: the registry resolves overlays, it
/// does not mirror direction or per-connection identity.
pub async fn connect_and_activate<B, A>(a: &mut HarnessNode<B>, b: &mut HarnessNode<B>, activate: A)
where
    B: NetworkBehaviour + Send,
    B::ToSwarm: std::fmt::Debug,
    A: Fn(&mut B, PeerId, OverlayAddress, SwarmNodeType),
{
    // The homogeneous case is the heterogeneous one with a shared hook applied
    // to each side; `&activate` implements the per-side `FnOnce` bound.
    connect_and_activate_hetero(a, b, &activate, &activate).await;
}

/// Connect two seeded nodes whose behaviours differ, marking each peer active in
/// the other's registry and running a per-side activation hook.
///
/// A heterogeneous pair (a real behaviour against a bespoke probe handler, say)
/// cannot share one activation closure the way [`connect_and_activate`] does, so
/// each side takes its own; pass a no-op for a side that gates on nothing.
pub async fn connect_and_activate_hetero<B1, B2, A1, A2>(
    a: &mut HarnessNode<B1>,
    b: &mut HarnessNode<B2>,
    activate_a: A1,
    activate_b: A2,
) where
    B1: NetworkBehaviour + Send,
    B1::ToSwarm: std::fmt::Debug,
    B2: NetworkBehaviour + Send,
    B2::ToSwarm: std::fmt::Debug,
    A1: FnOnce(&mut B1, PeerId, OverlayAddress, SwarmNodeType),
    A2: FnOnce(&mut B2, PeerId, OverlayAddress, SwarmNodeType),
{
    listen_memory(&mut a.swarm).await;
    listen_memory(&mut b.swarm).await;
    a.swarm.connect(&mut b.swarm).await;

    activate_in_registry(&a.identities, b.peer_id, b.overlay);
    activate_in_registry(&b.identities, a.peer_id, a.overlay);

    activate_a(a.swarm.behaviour_mut(), b.peer_id, b.overlay, b.node_type);
    activate_b(b.swarm.behaviour_mut(), a.peer_id, a.overlay, a.node_type);
}

/// Drive two nodes until `predicate` holds or `timeout` elapses, returning
/// whether it held.
///
/// The deadline runs on the tokio clock, so under `#[tokio::test(start_paused =
/// true)]` an idle pair auto-advances time and the call resolves without real
/// waiting; under a live clock it bounds wall time. The predicate observes both
/// swarms after every event, so callers assert on accumulated state rather than
/// counting events.
///
/// Under `start_paused` always drive through these helpers: the
/// `libp2p_swarm_test` drive helpers run their deadline on a wall-clock timer
/// and burn the full real duration on a paused runtime.
pub async fn drive_until<B1, B2, P>(
    a: &mut HarnessNode<B1>,
    b: &mut HarnessNode<B2>,
    timeout: Duration,
    mut predicate: P,
) -> bool
where
    B1: NetworkBehaviour + Send,
    B1::ToSwarm: std::fmt::Debug,
    B2: NetworkBehaviour + Send,
    B2::ToSwarm: std::fmt::Debug,
    P: FnMut(&Swarm<B1>, &Swarm<B2>) -> bool,
{
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        if predicate(&a.swarm, &b.swarm) {
            return true;
        }
        tokio::select! {
            () = &mut deadline => return predicate(&a.swarm, &b.swarm),
            _ = a.swarm.select_next_some() => {}
            _ = b.swarm.select_next_some() => {}
        }
    }
}

/// Drive two nodes for `duration`, polling both throughout.
///
/// Inspects neither swarm; use it to let a topology settle before asserting.
pub async fn drive_for<B1, B2>(a: &mut HarnessNode<B1>, b: &mut HarnessNode<B2>, duration: Duration)
where
    B1: NetworkBehaviour + Send,
    B1::ToSwarm: std::fmt::Debug,
    B2: NetworkBehaviour + Send,
    B2::ToSwarm: std::fmt::Debug,
{
    drive_until(a, b, duration, |_, _| false).await;
}

/// A source the [`drive`] loop can advance: a [`Swarm`], a [`HarnessNode`], or a
/// [`CommandPump`] adapter.
///
/// `poll_drive` advances the source once and reports `Ready` when it made
/// progress, so a driver races several sources without naming their transport.
/// The signature is runtime-free (no timer, no executor, no transport), so a
/// deterministic simulation can drive the same sources under virtual time; the
/// paused-clock deadline is the driver's concern, not the trait's.
pub trait DrivableSwarm {
    /// Poll the source once, returning `Ready` when it produced an event or
    /// otherwise made progress this poll.
    fn poll_drive(&mut self, cx: &mut Context<'_>) -> Poll<()>;
}

impl<B: NetworkBehaviour> DrivableSwarm for Swarm<B> {
    fn poll_drive(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        // The swarm stream never terminates; a yielded event is progress, its
        // payload discarded because callers assert on accumulated state.
        match self.poll_next_unpin(cx) {
            Poll::Ready(Some(_)) => Poll::Ready(()),
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
    }
}

impl<B: NetworkBehaviour> DrivableSwarm for HarnessNode<B> {
    fn poll_drive(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.swarm.poll_drive(cx)
    }
}

/// A [`DrivableSwarm`] that drains a command channel into a swarm's behaviour
/// before polling the swarm.
///
/// It folds a manual command pump into a driver's poll set, so a relay test that
/// feeds a node's own outbound commands back into it (or answers them) need not
/// hand-roll a recv-and-apply `select!` branch. Co-locating the channel with the
/// swarm in one owner keeps the pump out of the driver's terminal hook, which
/// already borrows the poll set.
pub struct CommandPump<'a, B: NetworkBehaviour, C, F> {
    swarm: &'a mut Swarm<B>,
    commands: &'a mut mpsc::Receiver<C>,
    apply: F,
}

impl<'a, B, C, F> CommandPump<'a, B, C, F>
where
    B: NetworkBehaviour,
    F: FnMut(&mut B, C),
{
    /// Drain `commands` into `swarm`'s behaviour through `apply` on every poll,
    /// then poll the swarm.
    pub fn new(swarm: &'a mut Swarm<B>, commands: &'a mut mpsc::Receiver<C>, apply: F) -> Self {
        Self {
            swarm,
            commands,
            apply,
        }
    }
}

impl<B, C, F> DrivableSwarm for CommandPump<'_, B, C, F>
where
    B: NetworkBehaviour,
    F: FnMut(&mut B, C),
{
    fn poll_drive(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let mut progressed = false;
        while let Poll::Ready(Some(command)) = self.commands.poll_recv(cx) {
            (self.apply)(self.swarm.behaviour_mut(), command);
            progressed = true;
        }
        // A drained command schedules swarm work that resolves on a later poll,
        // so report progress to keep the driver looping rather than parking.
        match self.swarm.poll_drive(cx) {
            Poll::Ready(()) => Poll::Ready(()),
            Poll::Pending if progressed => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Drive every source in `drivables`, calling `hook` after each poll round,
/// until the hook yields a value or `timeout` elapses; returns the hook's value,
/// or `None` on timeout.
///
/// The deadline runs on the tokio clock, so under `#[tokio::test(start_paused =
/// true)]` an idle set auto-advances rather than burning real time. The hook
/// observes external state (a response channel, a counter); fold a command
/// channel into `drivables` with a [`CommandPump`] rather than into the hook.
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
            () = &mut deadline => return hook(),
            () = step => {}
        }
    }
}

/// Drive `drivables` until `signal` resolves or `timeout` elapses, returning the
/// resolved value (or `None` on timeout).
///
/// The terminal condition every relay and completion test shares: a response
/// arrives on a oneshot while the swarms (and any [`CommandPump`]s) run.
pub async fn drive_until_signal<T>(
    drivables: &mut [&mut dyn DrivableSwarm],
    timeout: Duration,
    signal: &mut oneshot::Receiver<T>,
) -> Option<T> {
    drive(drivables, timeout, || signal.try_recv().ok()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::swarm::dummy;
    use vertex_net_peer_registry::ActivePeers;

    fn dummy_node(seed: u8) -> HarnessNode<dummy::Behaviour> {
        seeded_node(seed, SwarmNodeType::Client, |_| dummy::Behaviour)
    }

    #[test]
    fn seeded_identity_is_reproducible() {
        let a = seeded_identity(7, SwarmNodeType::Storer);
        let b = seeded_identity(7, SwarmNodeType::Storer);
        assert_eq!(a.peer_id, b.peer_id);
        assert_eq!(a.overlay, b.overlay);
        assert_eq!(a.peer_id, crate::test_peer_id(7));
        assert_eq!(a.overlay, test_overlay(7));

        let other = seeded_identity(8, SwarmNodeType::Storer);
        assert_ne!(a.peer_id, other.peer_id);
        assert_ne!(a.overlay, other.overlay);
    }

    #[tokio::test(start_paused = true)]
    async fn connect_and_activate_round_trip() {
        let mut a = dummy_node(1);
        let mut b = dummy_node(2);
        let (a_overlay, b_overlay) = (a.overlay, b.overlay);
        let (a_peer, b_peer) = (a.peer_id, b.peer_id);

        connect_and_activate(&mut a, &mut b, |_behaviour, _peer, _overlay, _node_type| {}).await;

        // Each side observes an established connection to the other.
        assert!(a.swarm.is_connected(&b_peer));
        assert!(b.swarm.is_connected(&a_peer));

        // Each registry now resolves the remote peer to its overlay, the read
        // path a behaviour routes commands through.
        assert_eq!(a.identities.active_id(&b_peer), Some(b_overlay));
        assert_eq!(b.identities.active_id(&a_peer), Some(a_overlay));
        assert_eq!(a.identities.active_peer_id(&b_overlay), Some(b_peer));
        assert_eq!(b.identities.active_peer_id(&a_overlay), Some(a_peer));
    }

    #[tokio::test(start_paused = true)]
    async fn drive_until_reports_deadline() {
        let mut a = dummy_node(3);
        let mut b = dummy_node(4);
        connect_and_activate(&mut a, &mut b, |_, _, _, _| {}).await;

        // A predicate that never holds returns false once the deadline lapses;
        // under start_paused the idle pair auto-advances so this is instant.
        let held = drive_until(&mut a, &mut b, Duration::from_secs(30), |_, _| false).await;
        assert!(!held);

        // A predicate already true returns immediately.
        let held = drive_until(&mut a, &mut b, Duration::from_secs(30), |_, _| true).await;
        assert!(held);
    }

    #[tokio::test(start_paused = true)]
    async fn drive_until_signal_resolves_or_times_out() {
        let mut a = dummy_node(9);
        let mut b = dummy_node(10);
        connect_and_activate(&mut a, &mut b, |_, _, _, _| {}).await;

        // A signal that never fires times out; the idle pair auto-advances.
        let (_tx, mut rx) = oneshot::channel::<u8>();
        {
            let mut drivables: [&mut dyn DrivableSwarm; 2] = [&mut a, &mut b];
            let out = drive_until_signal(&mut drivables, Duration::from_secs(5), &mut rx).await;
            assert_eq!(out, None);
        }

        // An already-fired signal resolves to its value.
        let (tx, mut rx) = oneshot::channel::<u8>();
        tx.send(42).unwrap();
        let mut drivables: [&mut dyn DrivableSwarm; 2] = [&mut a, &mut b];
        let out = drive_until_signal(&mut drivables, Duration::from_secs(5), &mut rx).await;
        assert_eq!(out, Some(42));
    }

    #[tokio::test(start_paused = true)]
    async fn command_pump_applies_before_polling() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let mut node = dummy_node(11);
        let (tx, mut rx) = mpsc::channel::<u8>(4);
        tx.try_send(1).unwrap();
        tx.try_send(2).unwrap();
        drop(tx);

        let applied = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&applied);
        let mut pump = CommandPump::new(
            &mut node.swarm,
            &mut rx,
            move |_behaviour: &mut dummy::Behaviour, command: u8| {
                sink.borrow_mut().push(command);
            },
        );

        let (_never_tx, mut never) = oneshot::channel::<()>();
        let mut drivables: [&mut dyn DrivableSwarm; 1] = [&mut pump];
        let out = drive_until_signal(&mut drivables, Duration::from_secs(1), &mut never).await;

        assert_eq!(out, None);
        assert_eq!(*applied.borrow(), vec![1, 2]);
    }
}
