//! AutoNAT v2 dial-back over a real TCP transport, in a vertex-shaped
//! behaviour composition.
//!
//! This exercises the wiring PR #156 added: vertex identify emits an
//! external-address candidate, the AutoNAT v2 client asks a peer's AutoNAT v2
//! server to dial it back, and a successful dial-back is mapped onto peer
//! reachability exactly as `handle_autonat_server_event` does in the node
//! event loop. It guards against an upstream libp2p change altering the autonat
//! event shapes our handlers destructure.

#![allow(clippy::expect_used)]

use std::time::Duration;

use futures::StreamExt;
use libp2p::autonat::v2 as autonat;
use libp2p::swarm::{Swarm, SwarmEvent};
use libp2p_swarm_test::SwarmExt;
use rand_08::rngs::OsRng;
use vertex_swarm_net_identify as identify;
use vertex_swarm_topology::{PeerReachability, ReachabilityTracker};

/// Minimal node mirroring the production composition: vertex identify (the
/// candidate source) plus the AutoNAT v2 client and server.
#[derive(libp2p::swarm::NetworkBehaviour)]
struct TestNode {
    identify: identify::Behaviour,
    autonat_client: autonat::client::Behaviour,
    autonat_server: autonat::server::Behaviour,
}

fn new_node() -> Swarm<TestNode> {
    Swarm::new_ephemeral_tokio(|keypair| TestNode {
        identify: identify::Behaviour::new(
            identify::Config::new(keypair.public().clone()),
            identify::new_agent_versions(),
        ),
        autonat_client: autonat::client::Behaviour::new(
            OsRng,
            autonat::client::Config::default().with_probe_interval(Duration::from_millis(100)),
        ),
        autonat_server: autonat::server::Behaviour::default(),
    })
}

#[tokio::test]
async fn autonat_v2_dialback_confirms_reachability() {
    let mut server = new_node();
    let mut client = new_node();

    server.listen().with_tcp_addr_external().await;
    client.listen().await;

    let client_peer = *client.local_peer_id();

    // The client dials the server. As the dialer (outbound, ephemeral source
    // port) the client's identify translates the server-observed address back
    // to its own listen address and emits it as an external-address candidate;
    // the AutoNAT client then asks the server to verify it by dialing back.
    client.connect(&mut server).await;

    let server_task = async {
        loop {
            if let SwarmEvent::Behaviour(TestNodeEvent::AutonatServer(event)) =
                server.select_next_some().await
                && event.client == client_peer
                && event.result.is_ok()
            {
                break event;
            }
        }
    };

    let client_task = async {
        loop {
            if let SwarmEvent::ExternalAddrConfirmed { .. } = client.select_next_some().await {
                break;
            }
        }
    };

    let (server_event, ()) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(server_task, client_task)
    })
    .await
    .expect("autonat v2 dial-back did not complete within 30s");

    // The successful dial-back proves the client peer is publicly reachable.
    // This is precisely what `handle_autonat_server_event` forwards into the
    // topology reachability tracker on an Ok result.
    let tracker = ReachabilityTracker::new();
    tracker.on_autonat_peer_confirmed(server_event.client);
    assert_eq!(tracker.status(&client_peer), PeerReachability::Reachable);
}
