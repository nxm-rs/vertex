//! Spawned task for gossip peer verification via dedicated lightweight swarm.
//!
//! Receives verification requests from topology, performs handshakes via an
//! isolated outbound-only swarm with a fresh random identity, and reports
//! results back via channel. Uses a separate identity to avoid invariant
//! violations (e.g., one overlay having multiple PeerIds).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::StreamExt;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent, dial_opts::DialOpts};
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmNodeType};
use vertex_swarm_identity::Identity;
use vertex_swarm_net_handshake::{
    HandshakeBehaviour, HandshakeEvent, NoAddresses,
};
use vertex_swarm_net_identify as identify;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_spec::Spec;

use super::verifier::{GossipVerifier, VerificationResult};

/// Request sent from topology to the verification task.
pub(crate) struct VerificationRequest {
    pub gossiper: OverlayAddress,
    pub peers: Vec<SwarmPeer>,
    /// Pre-fetched existing peer data for "already known" checks.
    pub existing: Vec<(OverlayAddress, Option<SwarmPeer>)>,
}

/// Event sent from the verification task back to topology.
#[derive(Debug)]
pub(crate) enum VerificationEvent {
    Verified {
        verified_peer: SwarmPeer,
        gossiper: OverlayAddress,
    },
    IdentityUpdated {
        verified_peer: SwarmPeer,
        gossiper: OverlayAddress,
    },
    DifferentPeerAtAddress {
        verified_peer: SwarmPeer,
        gossiped_overlay: OverlayAddress,
        gossiper: OverlayAddress,
    },
    Failed {
        gossiper: OverlayAddress,
        reason: super::verifier::VerificationFailureReason,
    },
    Unreachable {
        gossiper: OverlayAddress,
    },
}

/// Shared atomic metrics (read by topology, written by task).
pub(crate) struct VerifierMetrics {
    pub pending_count: Arc<AtomicUsize>,
    pub in_flight_count: Arc<AtomicUsize>,
    pub tracked_gossipers: Arc<AtomicUsize>,
    pub estimated_memory_bytes: Arc<AtomicUsize>,
}

impl VerifierMetrics {
    fn new() -> Self {
        Self {
            pending_count: Arc::new(AtomicUsize::new(0)),
            in_flight_count: Arc::new(AtomicUsize::new(0)),
            tracked_gossipers: Arc::new(AtomicUsize::new(0)),
            estimated_memory_bytes: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn update_from(&self, stats: &super::verifier::GossipVerifierStats) {
        self.pending_count.store(stats.pending_count, Ordering::Relaxed);
        self.in_flight_count.store(stats.in_flight_count, Ordering::Relaxed);
        self.tracked_gossipers.store(stats.tracked_gossipers, Ordering::Relaxed);
        self.estimated_memory_bytes.store(stats.estimated_memory_bytes, Ordering::Relaxed);
    }
}

impl Clone for VerifierMetrics {
    fn clone(&self) -> Self {
        Self {
            pending_count: self.pending_count.clone(),
            in_flight_count: self.in_flight_count.clone(),
            tracked_gossipers: self.tracked_gossipers.clone(),
            estimated_memory_bytes: self.estimated_memory_bytes.clone(),
        }
    }
}

/// Lightweight behaviour for verification handshakes + identify push.
///
/// Includes identify so that Go Bee's `waitPeerAddrs()` doesn't block ~10s
/// waiting for our addresses to appear in its peerstore.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "VerifierSwarmEvent")]
struct VerifierBehaviour {
    handshake: HandshakeBehaviour<Identity, NoAddresses>,
    identify: identify::Behaviour,
}

/// Events from the verifier swarm.
#[derive(Debug)]
enum VerifierSwarmEvent {
    Handshake(HandshakeEvent),
    Identify(identify::Event),
}

impl From<HandshakeEvent> for VerifierSwarmEvent {
    fn from(event: HandshakeEvent) -> Self {
        VerifierSwarmEvent::Handshake(event)
    }
}

impl From<identify::Event> for VerifierSwarmEvent {
    fn from(event: identify::Event) -> Self {
        VerifierSwarmEvent::Identify(event)
    }
}

/// Gossip verification task that owns a dedicated lightweight swarm.
struct GossipVerifierTask {
    request_rx: mpsc::Receiver<VerificationRequest>,
    result_tx: mpsc::Sender<VerificationEvent>,
    verifier: GossipVerifier,
    swarm: libp2p::Swarm<VerifierBehaviour>,
    metrics: VerifierMetrics,
}

impl GossipVerifierTask {
    async fn run(mut self) {
        loop {
            tokio::select! {
                Some(request) = self.request_rx.recv() => {
                    self.handle_request(request);
                    self.drain_pending_dials();
                    self.update_metrics();
                }
                event = self.swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                    self.update_metrics();
                }
                else => break,
            }
        }
        debug!("Gossip verifier task shutting down");
    }

    fn handle_request(&mut self, request: VerificationRequest) {
        let mut queued = 0;
        let mut skipped = 0;
        let mut rejected = 0;

        for (peer, (_, existing)) in request.peers.into_iter().zip(request.existing.into_iter()) {
            match self.verifier.check_gossip(peer, request.gossiper, existing.as_ref()) {
                super::verifier::GossipCheckResult::AlreadyKnown => {
                    skipped += 1;
                }
                super::verifier::GossipCheckResult::NeedsVerification(_) => {
                    queued += 1;
                }
                super::verifier::GossipCheckResult::Rejected(reason) => {
                    trace!(?reason, "Rejected gossiped peer");
                    rejected += 1;
                }
            }
        }

        if queued > 0 || rejected > 0 {
            debug!(
                gossiper = %request.gossiper,
                queued,
                skipped,
                rejected,
                "Verification request processed"
            );
        }
    }

    fn drain_pending_dials(&mut self) {
        while let Some(pending) = self.verifier.next_verification_dial() {
            let peer_id = pending.peer_id;
            let dial_addr = pending.dial_addr().clone();

            if self.swarm.is_connected(&peer_id) {
                self.verifier.on_verification_dial_failed(&peer_id);
                continue;
            }

            trace!(%peer_id, addr = %dial_addr, "Initiating verification dial");
            let opts = DialOpts::peer_id(peer_id)
                .addresses(vec![dial_addr])
                .build();

            if let Err(e) = self.swarm.dial(opts) {
                warn!(%peer_id, error = %e, "Failed to initiate verification dial");
                let result = self.verifier.on_verification_dial_failed(&peer_id);
                self.send_result(result);
            }
        }
    }

    fn handle_swarm_event(&mut self, event: SwarmEvent<VerifierSwarmEvent>) {
        match event {
            SwarmEvent::Behaviour(VerifierSwarmEvent::Handshake(
                HandshakeEvent::Completed { peer_id, info, .. },
            )) => {
                debug!(
                    %peer_id,
                    overlay = %info.swarm_peer.overlay(),
                    "Verification handshake completed"
                );
                let result = self.verifier.on_verification_handshake(peer_id, info.swarm_peer);
                self.send_result(result);

                // Disconnect after verification
                let _ = self.swarm.disconnect_peer_id(peer_id);

                // Drain more dials since a slot opened up
                self.drain_pending_dials();
            }
            SwarmEvent::Behaviour(VerifierSwarmEvent::Handshake(
                HandshakeEvent::Failed { peer_id, error, .. },
            )) => {
                debug!(%peer_id, %error, "Verification handshake failed");
                if self.verifier.is_in_flight(&peer_id) {
                    let result = self.verifier.on_verification_dial_failed(&peer_id);
                    self.send_result(result);
                }
                self.drain_pending_dials();
            }
            SwarmEvent::OutgoingConnectionError { peer_id: Some(peer_id), error, .. } => {
                debug!(%peer_id, %error, "Verification dial failed");
                if self.verifier.is_in_flight(&peer_id) {
                    let result = self.verifier.on_verification_dial_failed(&peer_id);
                    self.send_result(result);
                }
                self.drain_pending_dials();
            }
            SwarmEvent::Behaviour(VerifierSwarmEvent::Identify(
                identify::Event::Received { peer_id, info, .. },
            )) => {
                // Push our observed address back to the peer. This populates Go Bee's
                // peerstore so waitPeerAddrs() returns immediately instead of blocking ~10s.
                if !info.observed_addr.is_empty() {
                    trace!(%peer_id, observed = %info.observed_addr, "Pushing observed addr via identify");
                    self.swarm
                        .behaviour_mut()
                        .identify
                        .push_with_addresses(peer_id, vec![info.observed_addr]);
                }
            }
            SwarmEvent::Behaviour(VerifierSwarmEvent::Identify(_)) => {}
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                trace!(%peer_id, "Verification connection closed");
            }
            _ => {}
        }
    }

    fn send_result(&self, result: VerificationResult) {
        let event = match result {
            VerificationResult::Verified { verified_peer, gossiper } => {
                Some(VerificationEvent::Verified { verified_peer, gossiper })
            }
            VerificationResult::IdentityUpdated { verified_peer, gossiper } => {
                Some(VerificationEvent::IdentityUpdated { verified_peer, gossiper })
            }
            VerificationResult::DifferentPeerAtAddress { verified_peer, gossiped_overlay, gossiper } => {
                Some(VerificationEvent::DifferentPeerAtAddress {
                    verified_peer,
                    gossiped_overlay,
                    gossiper,
                })
            }
            VerificationResult::Failed { gossiper, reason } => {
                Some(VerificationEvent::Failed { gossiper, reason })
            }
            VerificationResult::Unreachable { gossiper } => {
                Some(VerificationEvent::Unreachable { gossiper })
            }
            VerificationResult::NotPending => None,
        };

        if let Some(event) = event {
            let _ = self.result_tx.try_send(event);
        }
    }

    fn update_metrics(&self) {
        self.metrics.update_from(&self.verifier.stats());
    }
}

/// Channel capacity for verification requests (bounded to prevent backpressure).
const REQUEST_CHANNEL_CAPACITY: usize = 64;

/// Channel capacity for verification results.
const RESULT_CHANNEL_CAPACITY: usize = 256;

/// Idle connection timeout for verification swarm.
const VERIFICATION_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Spawn the gossip verifier task with a dedicated lightweight swarm.
///
/// Creates a fresh random identity for the verification swarm to avoid
/// invariant violations (one overlay = one PeerId). The spec is inherited
/// from the main node's identity for network compatibility.
///
/// Returns channel endpoints and shared metrics. The task is spawned via the
/// `vertex-tasks` executor for graceful shutdown support.
pub(crate) fn spawn_gossip_verifier(
    spec: Arc<Spec>,
    local_overlay: OverlayAddress,
) -> Result<
    (
        mpsc::Sender<VerificationRequest>,
        mpsc::Receiver<VerificationEvent>,
        VerifierMetrics,
    ),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let (request_tx, request_rx) = mpsc::channel(REQUEST_CHANNEL_CAPACITY);
    let (result_tx, result_rx) = mpsc::channel(RESULT_CHANNEL_CAPACITY);

    let metrics = VerifierMetrics::new();
    let metrics_clone = metrics.clone();

    // Fresh random identity for the verification swarm.
    // Uses the same network spec for protocol compatibility.
    let verifier_identity = Arc::new(
        Identity::random(spec, SwarmNodeType::Client)
            .with_welcome_message("gossip-verifier"),
    );

    info!(
        overlay = %verifier_identity.overlay_address(),
        "Spawning gossip verifier with ephemeral identity"
    );

    // Build a lightweight outbound-only swarm with handshake + identify.
    // Identify is needed so Go Bee's waitPeerAddrs() doesn't block ~10s.
    // Uses NoAddresses since the verification swarm never listens.
    let swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .map_err(|e| format!("TCP transport: {e}"))?
        .with_dns()
        .map_err(|e| format!("DNS: {e}"))?
        .with_behaviour(|keypair| {
            let identify_config = identify::Config::new(keypair.public())
                .with_agent_version(format!("vertex-verifier/{}", env!("CARGO_PKG_VERSION")))
                .with_cache_size(0);
            Ok(VerifierBehaviour {
                handshake: HandshakeBehaviour::new(
                    verifier_identity.clone(),
                    Arc::new(NoAddresses),
                ),
                identify: identify::Behaviour::new(identify_config),
            })
        })
        .map_err(|e| format!("Behaviour: {e}"))?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(VERIFICATION_IDLE_TIMEOUT))
        .build();

    let verifier = GossipVerifier::new(local_overlay);

    let task = GossipVerifierTask {
        request_rx,
        result_tx,
        verifier,
        swarm,
        metrics: metrics_clone,
    };

    // Spawn using vertex-tasks for graceful shutdown support
    let executor = vertex_tasks::TaskExecutor::try_current()
        .map_err(|e| format!("No task executor available: {e}"))?;

    executor.spawn_critical_with_graceful_shutdown_signal(
        "gossip_verifier",
        |shutdown| async move {
            tokio::select! {
                _ = task.run() => {}
                guard = shutdown => {
                    drop(guard);
                }
            }
        },
    );

    Ok((request_tx, result_rx, metrics))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verifier_metrics_clone() {
        let metrics = VerifierMetrics::new();
        let cloned = metrics.clone();
        metrics.pending_count.store(42, Ordering::Relaxed);
        assert_eq!(cloned.pending_count.load(Ordering::Relaxed), 42);
    }
}
