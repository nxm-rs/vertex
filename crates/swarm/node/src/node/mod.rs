//! Node types for Swarm network participation.
//!
//! - [`BootNode`] - Topology only (bootnode servers); native-only.
//! - [`ClientNode`] - Topology + client protocols (chunk read/write).
//! - [`StorerNode`] - Client + storage protocols (chunk storage and staking);
//!   native-only.
//!
//! The browser client target builds only the [`ClientNode`] path. Bootnode and
//! storer are out of scope for `wasm32-unknown-unknown` (they need listeners,
//! NAT traversal, and native storage), so their modules are native-only.

mod base;
#[cfg(not(target_arch = "wasm32"))]
#[allow(unreachable_pub)]
mod bootnode;
mod builder;
#[allow(unreachable_pub)]
mod client;
mod core;
mod error;
mod launch;
// NAT traversal and LAN discovery only exist natively. The browser client
// dials over websockets and never listens, so the wasm sibling exposes the
// same item names and signatures over a no-op behaviour.
#[cfg_attr(target_arch = "wasm32", path = "nat_wasm.rs")]
mod nat;
pub(crate) mod stats;
#[cfg(all(not(target_arch = "wasm32"), feature = "storer"))]
#[allow(unreachable_pub)]
mod storer;
pub(crate) mod task;

pub use base::BaseNode;
#[cfg(not(target_arch = "wasm32"))]
pub use bootnode::{BootNode, BootNodeBuilder};
pub use builder::BuiltInfrastructure;
pub use client::{ClientNode, ClientNodeBuilder};
pub use core::{
    AssemblyContext, ClientCore, ClientCoreCtx, ClientCoreTail, ClientNodeParts, ClientTailParams,
    NativeChunkProvider, NativeDispatchEngine, NodeRunParts, NodeRunTaskFn, PseudosettleWiring,
    RunTaskFn, SettlementEventSenders, SharedAccounting, assemble_client_core, single_task,
    spawn_client_command_bridge,
};
#[cfg(feature = "swap")]
pub use core::{NodeChainError, SwapWiring, node_chain_provider};
pub use error::NodeBuildError;
pub use launch::{ClientLauncher, LaunchedClient};
#[cfg(all(not(target_arch = "wasm32"), feature = "storer"))]
pub use storer::{StorerNode, StorerNodeBuilder, StorerPullsyncControl};

/// Upper bound on queued items drained from a single channel per central-loop
/// wake, after the first item the `select!` already delivered. Bounding the
/// burst keeps the swarm poll from being starved: once the budget is spent the
/// loop returns to `select!`, which polls the swarm again before the next drain.
/// A bulk download fans many retrieval legs, settles, and acks into the command
/// channel at once, so admitting a burst per wake (rather than one per full
/// select pass) keeps the single central task from serialising on wake latency.
pub(crate) const CHANNEL_DRAIN_BUDGET: usize = 32;

/// A handshake-completion hook seeding a peer's accounting serve line from its
/// advertised node type and returning the line now in force. Erased as
/// `Arc<dyn Fn>` so the node struct stays free of the accounting type
/// parameters; wired from the accounting that `enable_forwarding` already
/// receives.
pub(crate) type AccountingConnect = std::sync::Arc<
    dyn Fn(
            vertex_swarm_primitives::OverlayAddress,
            vertex_swarm_primitives::SwarmNodeType,
        ) -> vertex_swarm_api::Au
        + Send
        + Sync,
>;

/// Commands for a peer whose handshake just completed: activation, then the
/// initial payment-threshold announcement when accounting is wired. The connect
/// hook runs first (a dispatch task may already have created the peer lazily on
/// the client line) and the announcement carries the serve line it returns, so
/// the peer adopts exactly the line the provide gate enforces. One initial
/// announcement per connection; repayment past a growth checkpoint re-announces
/// the raised line from the settlement path.
pub(crate) fn peer_ready_commands(
    accounting_connect: Option<&AccountingConnect>,
    peer_id: libp2p::PeerId,
    overlay: vertex_swarm_primitives::OverlayAddress,
    node_type: vertex_swarm_primitives::SwarmNodeType,
) -> Vec<vertex_swarm_client_protocol::ClientCommand> {
    use vertex_swarm_api::Au;
    use vertex_swarm_client_protocol::{ClientCommand, PeerCommand};

    let mut commands = vec![ClientCommand::ActivatePeer {
        peer_id,
        overlay,
        node_type,
    }];
    if let Some(connect) = accounting_connect {
        let serve_line = connect(overlay, node_type);
        // Announce only a positive line: a peer rejects a zero announcement as
        // below its minimum and disconnects, while silence is tolerated.
        if serve_line > Au::ZERO
            && let Ok(threshold) = alloy_primitives::U256::try_from(serve_line)
        {
            commands.push(ClientCommand::Peer {
                peer: overlay,
                command: PeerCommand::AnnouncePaymentThreshold { threshold },
            });
        }
    }
    commands
}

#[cfg(test)]
// Asserting on positional command output reads clearest with direct indexing;
// the CI test lint profile permits it in test code.
#[allow(clippy::indexing_slicing)]
mod announce_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use alloy_primitives::U256;
    use libp2p::PeerId;
    use vertex_swarm_api::Au;
    use vertex_swarm_client_protocol::{ClientCommand, PeerCommand};
    use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

    use super::{AccountingConnect, peer_ready_commands};

    fn recording_connect(line: Au, calls: Arc<AtomicUsize>) -> AccountingConnect {
        Arc::new(move |_overlay, _node_type| {
            calls.fetch_add(1, Ordering::SeqCst);
            line
        })
    }

    #[test]
    fn peer_ready_activates_then_announces_the_connect_seeded_line() {
        let calls = Arc::new(AtomicUsize::new(0));
        let connect = recording_connect(Au::from_amount(13_500_000), Arc::clone(&calls));
        let peer_id = PeerId::random();
        let overlay = OverlayAddress::from([7u8; 32]);

        let commands = peer_ready_commands(Some(&connect), peer_id, overlay, SwarmNodeType::Storer);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(commands.len(), 2);
        assert!(matches!(
            &commands[0],
            ClientCommand::ActivatePeer { peer_id: p, overlay: o, node_type: SwarmNodeType::Storer }
                if *p == peer_id && *o == overlay
        ));
        assert!(matches!(
            &commands[1],
            ClientCommand::Peer { peer, command: PeerCommand::AnnouncePaymentThreshold { threshold } }
                if *peer == overlay && *threshold == U256::from(13_500_000u64)
        ));
    }

    #[test]
    fn peer_ready_without_accounting_only_activates() {
        let commands = peer_ready_commands(
            None,
            PeerId::random(),
            OverlayAddress::from([8u8; 32]),
            SwarmNodeType::Client,
        );

        assert_eq!(commands.len(), 1);
        assert!(matches!(commands[0], ClientCommand::ActivatePeer { .. }));
    }

    #[test]
    fn peer_ready_never_announces_a_zero_line() {
        let calls = Arc::new(AtomicUsize::new(0));
        let connect = recording_connect(Au::ZERO, Arc::clone(&calls));

        let commands = peer_ready_commands(
            Some(&connect),
            PeerId::random(),
            OverlayAddress::from([9u8; 32]),
            SwarmNodeType::Client,
        );

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(commands.len(), 1);
        assert!(matches!(commands[0], ClientCommand::ActivatePeer { .. }));
    }
}
