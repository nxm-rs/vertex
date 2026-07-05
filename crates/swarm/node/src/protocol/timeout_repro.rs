//! Repro for the retrieval/pushsync liveness invariant.
//!
//! A peer can negotiate the retrieval substream and its headers and then simply
//! never write the delivery frame. Without a per-request deadline the caller's
//! outbound future would block on that read forever. This module drives that
//! exact scenario through the swarm-test harness: a real [`ClientBehaviour`]
//! requester against a deliberately withholding server that completes the header
//! exchange, captures the [`RetrievalResponder`], and drops it on the floor
//! without responding.
//!
//! The gate: the outbound retrieval resolves with
//! [`ChunkTransferError::TimedOut`] within the configured `retrieval_timeout`,
//! not after the shared 30s `timeout`, and not never. The test sets a short
//! `retrieval_timeout` and asserts the attempt errors well under a wall-clock
//! guard, so a regression that unbounds the read (or re-routes it onto the
//! shared deadline) fails here.

use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::StreamExt;
use libp2p::core::transport::PortUse;
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::swarm::{
    ConnectionDenied, ConnectionHandler, ConnectionHandlerEvent, ConnectionId, FromSwarm,
    NetworkBehaviour, SubstreamProtocol, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
    handler::{ConnectionEvent, FullyNegotiatedInbound},
};
use nectar_primitives::ChunkAddress;
use tokio::sync::oneshot;
use vertex_swarm_api::SwarmLocalStore;
use vertex_swarm_localstore::ChunkStore;
use vertex_swarm_net_retrieval::{RetrievalInboundProtocol, RetrievalResponder, inbound};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_test_utils::harness::{HarnessNode, connect_and_activate_hetero, seeded_node};

use crate::ChunkTransferError;
use crate::client_service::RetrievalResult;
use crate::protocol::{
    BehaviourConfig, ClientBehaviour, ClientCommand, PeerCommand, StubForwarder,
};

/// A connection handler that accepts a single retrieval substream, completes the
/// header exchange (the `RetrievalInboundProtocol` upgrade reads the request and
/// replies with response headers), and then holds the responder without ever
/// sending a delivery. This is the withholding peer.
#[derive(Default)]
struct WithholdingHandler {
    /// Captured responders, held forever so the requester's read blocks.
    held: Vec<RetrievalResponder>,
}

impl ConnectionHandler for WithholdingHandler {
    type FromBehaviour = ();
    type ToBehaviour = ();
    type InboundProtocol = RetrievalInboundProtocol;
    type OutboundProtocol = libp2p::core::upgrade::DeniedUpgrade;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        // No timeout on the inbound side: the server intends to stall, so it must
        // not be the one to drop the substream. The requester's outbound deadline
        // is what bounds the exchange.
        SubstreamProtocol::new(inbound(), ())
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        Poll::Pending
    }

    fn on_behaviour_event(&mut self, _event: Self::FromBehaviour) {}

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        if let ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
            protocol: (_request, responder),
            ..
        }) = event
        {
            // Capture the responder and never reply: the request and headers
            // negotiated, but no delivery frame is ever written.
            self.held.push(responder);
        }
    }
}

/// A behaviour that installs a [`WithholdingHandler`] on every connection.
#[derive(Default)]
struct WithholdingBehaviour;

impl NetworkBehaviour for WithholdingBehaviour {
    type ConnectionHandler = WithholdingHandler;
    type ToSwarm = ();

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: libp2p::PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(WithholdingHandler::default())
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: libp2p::PeerId,
        _addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(WithholdingHandler::default())
    }

    fn on_swarm_event(&mut self, _event: FromSwarm<'_>) {}

    fn on_connection_handler_event(
        &mut self,
        _peer_id: libp2p::PeerId,
        _connection_id: ConnectionId,
        _event: THandlerOutEvent<Self>,
    ) {
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}

/// Build a requester `ClientBehaviour` with a short retrieval deadline so the
/// withholding peer is bounded in milliseconds, not the shared 30s default.
fn requester_node(seed: u8, retrieval_timeout: Duration) -> HarnessNode<ClientBehaviour> {
    seeded_node(seed, SwarmNodeType::Client, move |ctx| {
        let mut config = BehaviourConfig::for_role(SwarmNodeType::Client);
        config.handler.retrieval_timeout = retrieval_timeout;
        let store: Arc<dyn SwarmLocalStore> = Arc::new(ChunkStore::with_budget(1 << 20, 1_000));
        ClientBehaviour::new(
            config,
            store,
            Arc::new(StubForwarder),
            ctx.identities.clone(),
        )
    })
}

// Paused time: the libp2p substream deadline rides the tokio clock on native,
// so the whole reproduction resolves on logical time with no wall-clock wait.
#[tokio::test(start_paused = true)]
async fn withholding_peer_resolves_as_timed_out_within_the_deadline() {
    // A 200ms retrieval deadline: short enough to assert against, far below the
    // shared 30s default that the bug would otherwise impose.
    let retrieval_timeout = Duration::from_millis(200);

    let mut requester = requester_node(1, retrieval_timeout);
    let mut server = seeded_node(2, SwarmNodeType::Client, |_| WithholdingBehaviour);
    // The requester must know the server's overlay to dispatch the request; the
    // server never validates it, so the harness-seeded overlay is arbitrary.
    let server_overlay = server.overlay;

    // The withholding server gates on nothing, so only the requester takes an
    // activation hook (mark the server Active, as the connection registry would
    // at handshake completion).
    connect_and_activate_hetero(
        &mut requester,
        &mut server,
        |behaviour, peer_id, overlay, node_type| {
            behaviour.on_command(ClientCommand::ActivatePeer {
                peer_id,
                overlay,
                node_type,
            });
        },
        |_, _, _, _| {},
    )
    .await;

    let address = ChunkAddress::new([0x11; 32]);
    let (tx, mut rx) = oneshot::channel::<Result<RetrievalResult, ChunkTransferError>>();
    requester
        .swarm
        .behaviour_mut()
        .on_command(ClientCommand::Peer {
            peer: server_overlay,
            command: PeerCommand::RetrieveChunk {
                address,
                response: tx,
                originated: true,
            },
        });

    let start = Instant::now();
    let drive = async {
        loop {
            tokio::select! {
                _ = requester.swarm.select_next_some() => {}
                _ = server.swarm.select_next_some() => {}
                res = &mut rx => return res.expect("sender not dropped"),
            }
        }
    };

    // A wall-clock guard far below the shared 30s default but generous over the
    // 200ms deadline: if the read were unbounded (or on the shared timeout) this
    // outer guard would fire instead of the per-request deadline.
    let outcome = tokio::time::timeout(Duration::from_secs(5), drive)
        .await
        .expect("the retrieval must resolve via its own deadline, not hang");
    let elapsed = start.elapsed();

    assert!(
        matches!(outcome, Err(ChunkTransferError::TimedOut)),
        "a withholding peer must resolve the attempt as TimedOut, got {outcome:?}"
    );
    // Strictly tighter than the 5s guard above: 2s is a 10x margin over the
    // 200ms deadline, so resolving under it proves the per-request deadline
    // fired rather than any shared or outer timeout.
    assert!(
        elapsed < Duration::from_secs(2),
        "the attempt resolved on its retrieval_timeout, not the wall-clock guard ({elapsed:?})"
    );

    // The typed timeout is retryable: the get path is free to race the next
    // candidate rather than treating the withholding peer as terminal.
    assert!(
        ChunkTransferError::TimedOut.is_retryable(),
        "a timeout must be retryable so the requester moves to the next candidate"
    );
}
