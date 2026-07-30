//! Two-host proof over the sim world: real vertex swarms complete the
//! handshake protocol on the simulated network, and the seeded world trace is
//! stable across in-process runs.
#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use libp2p::{PeerId, swarm::SwarmEvent};
use vertex_swarm_identity::Identity;
use vertex_swarm_net_handshake::{HandshakeBehaviour, HandshakeEvent, NoAddresses};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_sim::{
    HostContext, HostResult, SimAuth, SimWorld, TracedSwarm, host_keypair, listen_multiaddr,
};
use vertex_swarm_test_utils::test_spec;

const PORT: u16 = 1634;

type Behaviour = HandshakeBehaviour<Identity, NoAddresses>;

/// Normalize a handshake event; `ConnectionId`s are process-global and stay
/// out of the trace.
fn handshake_summary(event: &HandshakeEvent) -> String {
    match event {
        HandshakeEvent::Completed {
            peer_id,
            direction,
            info,
            ..
        } => format!(
            "handshake-completed peer={peer_id} overlay={} node_type={:?} direction={direction:?}",
            info.swarm_peer.overlay(),
            info.node_type,
        ),
        HandshakeEvent::Failed { peer_id, error, .. } => {
            format!("handshake-failed peer={peer_id} error={error}")
        }
    }
}

fn build_traced(ctx: &HostContext, auth: SimAuth) -> TracedSwarm<Behaviour> {
    let identity = Arc::new(ctx.identity(test_spec(), SwarmNodeType::Client));
    let swarm = ctx.swarm(auth, move |_keypair| {
        HandshakeBehaviour::new(identity, Arc::new(NoAddresses), "sim")
    });
    ctx.traced(swarm, handshake_summary)
}

fn is_completed(event: &SwarmEvent<HandshakeEvent>) -> bool {
    matches!(
        event,
        SwarmEvent::Behaviour(HandshakeEvent::Completed { .. })
    )
}

/// Drive until the local handshake completes, then keep driving for a fixed
/// virtual linger so the remote side can finish too.
async fn drive_to_completion(traced: &mut TracedSwarm<Behaviour>) -> HostResult {
    if !traced
        .drive_until(Duration::from_secs(60), is_completed)
        .await
    {
        return Err("handshake did not complete".into());
    }
    traced.drive_for(Duration::from_secs(2)).await;
    Ok(())
}

/// Run the two-host handshake sim and return the world trace.
///
/// The seed is fixed, not replayable: these tests derive identities from the
/// literal and compare worlds built from distinct seeds.
fn run_sim(seed: u64, auth: SimAuth) -> Vec<String> {
    let mut world = SimWorld::builder()
        .fixed_seed(seed)
        .duration(Duration::from_secs(120))
        .build();

    world.client("server", move |ctx| async move {
        let mut traced = build_traced(&ctx, auth);
        traced.swarm_mut().listen_on(listen_multiaddr(PORT))?;
        drive_to_completion(&mut traced).await
    });

    world.client("dialer", move |ctx| async move {
        let mut traced = build_traced(&ctx, auth);
        // Fixed virtual delay so the server is always listening first.
        tokio::time::sleep(Duration::from_millis(200)).await;
        traced
            .swarm_mut()
            .dial(ctx.lookup_multiaddr("server", PORT))?;
        drive_to_completion(&mut traced).await
    });

    world.run();
    world.trace_lines()
}

fn completed_line(trace: &[String], host: &str, remote: PeerId) -> bool {
    trace.iter().any(|l| {
        l.contains(host) && l.contains("handshake-completed") && l.contains(&remote.to_string())
    })
}

/// Both sides complete the real handshake over the production-shaped noise
/// plus yamux stack running on simulated TCP.
#[test]
fn handshake_round_trip_over_noise() {
    let trace = run_sim(42, SimAuth::Noise);
    let server_id = host_keypair(42, "server").public().to_peer_id();
    let dialer_id = host_keypair(42, "dialer").public().to_peer_id();
    assert!(
        completed_line(&trace, "server", dialer_id),
        "server never completed: {trace:#?}"
    );
    assert!(
        completed_line(&trace, "dialer", server_id),
        "dialer never completed: {trace:#?}"
    );
}

/// The identical seeded sim, run twice in one process, produces identical
/// world traces on the entropy-free plaintext stack.
#[test]
fn plaintext_trace_is_seed_stable_in_process() {
    let first = run_sim(7, SimAuth::Plaintext);
    let second = run_sim(7, SimAuth::Plaintext);
    assert!(
        first.iter().any(|l| l.contains("handshake-completed")),
        "no handshake completed: {first:#?}"
    );
    assert_eq!(first, second, "seeded traces diverged");
}

/// Noise draws ephemeral keys from the OS RNG, but that entropy never feeds
/// the scheduler, so the world trace is still seed-stable in-process.
#[test]
fn noise_trace_is_seed_stable_in_process() {
    let first = run_sim(9, SimAuth::Noise);
    let second = run_sim(9, SimAuth::Noise);
    assert_eq!(first, second, "seeded noise traces diverged");
}

/// A different sim seed changes the derived identities and the virtual
/// message schedule, proving the seed drives the whole world.
#[test]
fn different_seed_changes_the_schedule() {
    let a = run_sim(7, SimAuth::Plaintext);
    let b = run_sim(8, SimAuth::Plaintext);
    assert_ne!(a, b, "distinct seeds produced identical traces");
}
