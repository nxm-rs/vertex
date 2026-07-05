//! Vertex harness over `libp2p-swarm-test`.
//!
//! Behaviour tests re-roll the same plumbing: an ephemeral swarm, a memory
//! listener made external, a connect, then a hand-marked active peer in the
//! connection registry the behaviour reads through. This module folds that into
//! seeded identities, a first-class [`connect_and_activate`], `start_paused`
//! aware drive helpers, and panic-safe spawning.
//!
//! Transport is an implementation detail: nodes run on the in-process memory
//! transport, but no test-facing signature names it, so a later deterministic
//! simulation can slot under the same surface.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{ConnectionId, NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, Swarm};
use libp2p_swarm_test::SwarmExt;
use vertex_net_peer_registry::PeerRegistry;
use vertex_swarm_api::SwarmNodeType;
use vertex_swarm_primitives::OverlayAddress;
use vertex_tasks::{TaskExecutor, TaskHandle};

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
/// its per-connection indices stay unique.
fn next_connection_id() -> ConnectionId {
    static N: AtomicUsize = AtomicUsize::new(1);
    ConnectionId::new_unchecked(N.fetch_add(1, Ordering::Relaxed))
}

/// Mark `peer`/`overlay` active in `registry`, the transition topology performs
/// at handshake completion (an inbound connection promoted to a known overlay).
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
pub async fn connect_and_activate<B, A>(a: &mut HarnessNode<B>, b: &mut HarnessNode<B>, activate: A)
where
    B: NetworkBehaviour + Send,
    B::ToSwarm: std::fmt::Debug,
    A: Fn(&mut B, PeerId, OverlayAddress, SwarmNodeType),
{
    listen_memory(&mut a.swarm).await;
    listen_memory(&mut b.swarm).await;
    a.swarm.connect(&mut b.swarm).await;

    activate_in_registry(&a.identities, b.peer_id, b.overlay);
    activate_in_registry(&b.identities, a.peer_id, a.overlay);

    activate(a.swarm.behaviour_mut(), b.peer_id, b.overlay, b.node_type);
    activate(b.swarm.behaviour_mut(), a.peer_id, a.overlay, a.node_type);
}

/// Drive two nodes until `predicate` holds or `timeout` elapses, returning
/// whether it held.
///
/// The deadline runs on the tokio clock, so under `#[tokio::test(start_paused =
/// true)]` an idle pair auto-advances time and the call resolves without real
/// waiting; under a live clock it bounds wall time. The predicate observes both
/// swarms after every event, so callers assert on accumulated state rather than
/// counting events.
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

/// Spawn a background task on the current [`TaskExecutor`] as critical, so a
/// panic notifies the [`TaskManager`](vertex_tasks::TaskManager) instead of
/// vanishing silently.
///
/// Requires a task executor to be current (install one with
/// [`TaskManager::current`](vertex_tasks::TaskManager::current)).
pub fn spawn<F>(name: &'static str, fut: F) -> TaskHandle
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    TaskExecutor::current().spawn_critical(name, fut)
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
}
