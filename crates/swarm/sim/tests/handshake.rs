//! Two-host turmoil proof: real vertex swarms complete the handshake protocol
//! over the simulated network, and the seeded event trace is stable across
//! in-process runs.
#![allow(clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_primitives::B256;
use alloy_signer_local::LocalSigner;
use futures::StreamExt;
use libp2p::{Multiaddr, PeerId, Swarm, swarm::SwarmEvent};
use vertex_swarm_identity::Identity;
use vertex_swarm_net_handshake::{HandshakeBehaviour, HandshakeEvent, NoAddresses};
use vertex_swarm_primitives::{Nonce, SwarmNodeType};
use vertex_swarm_sim::{noise_stack, plaintext_stack};
use vertex_swarm_test_utils::{test_keypair, test_spec};

const PORT: u16 = 1634;
const SERVER_SEED: u8 = 1;
const DIALER_SEED: u8 = 2;

type Trace = Arc<Mutex<Vec<String>>>;

#[derive(Clone, Copy)]
enum Auth {
    Plaintext,
    Noise,
}

/// Build a swarm with a fully seeded identity: libp2p keypair, overlay
/// signer, and nonce all derive from one seed byte, so no OS entropy is
/// drawn on the plaintext path.
fn build_swarm(seed: u8, auth: Auth) -> Swarm<HandshakeBehaviour<Identity, NoAddresses>> {
    let keypair = test_keypair(seed);
    let peer_id = keypair.public().to_peer_id();
    let signer = LocalSigner::from_bytes(&B256::repeat_byte(seed)).expect("non-zero secret key");
    let identity = Identity::new(signer, Nonce::ZERO, test_spec(), SwarmNodeType::Client);
    let behaviour = HandshakeBehaviour::new(Arc::new(identity), Arc::new(NoAddresses), "sim");
    let transport = match auth {
        Auth::Plaintext => plaintext_stack(&keypair),
        Auth::Noise => noise_stack(&keypair).expect("noise config from a valid keypair"),
    };
    Swarm::new(
        transport,
        behaviour,
        peer_id,
        libp2p::swarm::Config::with_tokio_executor()
            .with_idle_connection_timeout(Duration::from_secs(60)),
    )
}

/// Append a normalized line for `event` to the shared trace.
///
/// `ConnectionId`s come from a process-global counter, so they are excluded;
/// everything else (virtual time, peer ids, addresses, handshake payload
/// summary) is asserted byte-for-byte across runs.
fn record(host: &str, trace: &Trace, event: &SwarmEvent<HandshakeEvent>) {
    let summary = match event {
        SwarmEvent::NewListenAddr { address, .. } => format!("listen {address}"),
        SwarmEvent::IncomingConnection { send_back_addr, .. } => {
            format!("incoming from {send_back_addr}")
        }
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
        } => format!(
            "established peer={peer_id} addr={}",
            endpoint.get_remote_address()
        ),
        SwarmEvent::ConnectionClosed { peer_id, .. } => format!("closed peer={peer_id}"),
        SwarmEvent::Behaviour(HandshakeEvent::Completed {
            peer_id,
            direction,
            info,
            ..
        }) => format!(
            "handshake-completed peer={peer_id} overlay={} node_type={:?} direction={direction:?}",
            info.swarm_peer.overlay(),
            info.node_type,
        ),
        SwarmEvent::Behaviour(HandshakeEvent::Failed { peer_id, error, .. }) => {
            format!("handshake-failed peer={peer_id} error={error}")
        }
        SwarmEvent::Dialing { .. } => "dialing".to_string(),
        SwarmEvent::NewExternalAddrCandidate { address } => format!("addr-candidate {address}"),
        _ => "other".to_string(),
    };
    trace.lock().expect("trace mutex").push(format!(
        "[{}ms {host}] {summary}",
        turmoil::elapsed().as_millis()
    ));
}

fn is_completed(event: &SwarmEvent<HandshakeEvent>) -> bool {
    matches!(
        event,
        SwarmEvent::Behaviour(HandshakeEvent::Completed { .. })
    )
}

/// Drive `swarm` until its handshake completes, then keep driving for a
/// fixed virtual linger so the remote side can finish too.
async fn drive_to_completion(
    host: &str,
    trace: &Trace,
    swarm: &mut Swarm<HandshakeBehaviour<Identity, NoAddresses>>,
) {
    loop {
        let event = swarm.select_next_some().await;
        record(host, trace, &event);
        if is_completed(&event) {
            break;
        }
    }
    let linger = tokio::time::sleep(Duration::from_secs(2));
    tokio::pin!(linger);
    loop {
        // `biased` keeps branch polling order fixed: unbiased `select!` draws
        // from tokio's thread-local RNG, which is not covered by the sim seed.
        tokio::select! {
            biased;
            _ = &mut linger => break,
            event = swarm.select_next_some() => record(host, trace, &event),
        }
    }
}

/// Run the two-host handshake sim and return the merged event trace.
fn run_sim(seed: u64, auth: Auth) -> Vec<String> {
    let trace: Trace = Arc::default();

    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(120))
        .rng_seed(seed)
        .build();

    let server_trace = Arc::clone(&trace);
    sim.client("server", async move {
        let mut swarm = build_swarm(SERVER_SEED, auth);
        swarm.listen_on(format!("/ip4/0.0.0.0/tcp/{PORT}").parse::<Multiaddr>()?)?;
        drive_to_completion("server", &server_trace, &mut swarm).await;
        Ok(())
    });

    let dialer_trace = Arc::clone(&trace);
    sim.client("dialer", async move {
        let mut swarm = build_swarm(DIALER_SEED, auth);
        // Fixed virtual delay so the server is always listening first.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let server_ip = turmoil::lookup("server");
        let addr: Multiaddr = format!("/ip4/{server_ip}/tcp/{PORT}").parse()?;
        swarm.dial(addr)?;
        drive_to_completion("dialer", &dialer_trace, &mut swarm).await;
        Ok(())
    });

    sim.run().expect("simulation completes without host errors");

    Arc::try_unwrap(trace)
        .expect("sim dropped all trace handles")
        .into_inner()
        .expect("trace mutex")
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
    let trace = run_sim(42, Auth::Noise);
    let server_id = test_keypair(SERVER_SEED).public().to_peer_id();
    let dialer_id = test_keypair(DIALER_SEED).public().to_peer_id();
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
/// event traces on the entropy-free plaintext stack.
#[test]
fn plaintext_trace_is_seed_stable_in_process() {
    let first = run_sim(7, Auth::Plaintext);
    let second = run_sim(7, Auth::Plaintext);
    assert!(
        first.iter().any(|l| l.contains("handshake-completed")),
        "no handshake completed: {first:#?}"
    );
    assert_eq!(first, second, "seeded traces diverged");
}

/// Noise draws ephemeral keys from the OS RNG, but that entropy never feeds
/// the scheduler, so the normalized event trace is still seed-stable
/// in-process.
#[test]
fn noise_trace_is_seed_stable_in_process() {
    let first = run_sim(9, Auth::Noise);
    let second = run_sim(9, Auth::Noise);
    assert_eq!(first, second, "seeded noise traces diverged");
}

/// A different sim seed shifts the virtual message latencies, proving the
/// seed actually drives the network schedule.
#[test]
fn different_seed_changes_the_schedule() {
    let a = run_sim(7, Auth::Plaintext);
    let b = run_sim(8, Auth::Plaintext);
    assert_ne!(a, b, "distinct seeds produced identical traces");
}
