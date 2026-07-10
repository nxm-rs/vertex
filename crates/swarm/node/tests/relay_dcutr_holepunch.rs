//! Circuit relay v2 reservation and DCUtR hole punch over real TCP
//! transports, in a vertex-shaped behaviour composition.
//!
//! This exercises the relay and DCUtR wiring: a relay server grants a
//! reservation to a NATed listener, a second NATed client dials the listener
//! through the relayed circuit, and DCUtR coordinates the simultaneous open
//! that upgrades the circuit to a direct connection. It guards against an
//! upstream libp2p change altering the relay and dcutr event shapes that
//! `handle_relay_event`, `handle_relay_client_event`, and `handle_dcutr_event`
//! destructure in the node event loop.

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::expect_used)]

use std::time::Duration;

use futures::StreamExt;
use libp2p::core::multiaddr::Protocol;
use libp2p::swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use libp2p::{Multiaddr, dcutr, noise, relay, tcp, yamux};
use libp2p_swarm_test::SwarmExt;
use vertex_swarm_net_identify as identify;

/// Relay server mirroring the publicly reachable composition: vertex identify
/// (clients learn their observed addresses from it) plus the circuit relay v2
/// server.
#[derive(NetworkBehaviour)]
struct RelayNode {
    identify: identify::Behaviour,
    relay: relay::Behaviour,
}

/// NATed client mirroring the production composition: vertex identify (the
/// external-address candidate source DCUtR advertises from), the relay client
/// paired with the circuit transport, and DCUtR.
#[derive(NetworkBehaviour)]
struct ClientNode {
    identify: identify::Behaviour,
    relay_client: relay::client::Behaviour,
    dcutr: dcutr::Behaviour,
}

fn new_identify(keypair: &libp2p::identity::Keypair) -> identify::Behaviour {
    identify::Behaviour::new(
        identify::Config::new(keypair.public()),
        identify::new_agent_versions(),
        identify::ObservedAddresses::default(),
    )
}

/// Relay server over the production TCP, Noise, and Yamux suite.
fn new_relay_node() -> Swarm<RelayNode> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("TCP transport builds")
        .with_behaviour(|keypair| RelayNode {
            identify: new_identify(keypair),
            relay: relay::Behaviour::new(keypair.public().to_peer_id(), relay::Config::default()),
        })
        .expect("behaviour builds")
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
        .build()
}

/// NATed client over the same suite with the circuit relay client transport
/// layered on top, matching the node's swarm assembly.
fn new_client_node() -> Swarm<ClientNode> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("TCP transport builds")
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .expect("relay client transport builds")
        .with_behaviour(|keypair, relay_client| ClientNode {
            identify: new_identify(keypair),
            relay_client,
            dcutr: dcutr::Behaviour::new(keypair.public().to_peer_id()),
        })
        .expect("behaviour builds")
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
        .build()
}

/// Listen on an ephemeral loopback TCP port and return the bound multiaddr.
async fn listen_tcp<B>(swarm: &mut Swarm<B>) -> Multiaddr
where
    B: NetworkBehaviour + Send,
    B::ToSwarm: std::fmt::Debug,
{
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("valid multiaddr"))
        .expect("listen on loopback TCP");
    swarm
        .wait(|event| match event {
            SwarmEvent::NewListenAddr { address, .. } => Some(address),
            _ => None,
        })
        .await
}

#[tokio::test]
async fn relay_reservation_and_dcutr_holepunch() {
    let mut relay_node = new_relay_node();
    let mut listener = new_client_node();
    let mut dialer = new_client_node();

    let relay_addr = listen_tcp(&mut relay_node).await;
    relay_node.add_external_address(relay_addr.clone());
    let listener_addr = listen_tcp(&mut listener).await;
    listen_tcp(&mut dialer).await;

    let relay_peer_id = *relay_node.local_peer_id();
    let listener_peer_id = *listener.local_peer_id();

    // Listening on the circuit address asks the relay for a reservation; the
    // relay client transport dials the relay under the hood.
    let relayed_addr = relay_addr
        .with(Protocol::P2p(relay_peer_id))
        .with(Protocol::P2pCircuit)
        .with(Protocol::P2p(listener_peer_id));
    listener
        .listen_on(relayed_addr.clone())
        .expect("listen on the relayed circuit address");

    // Reservation proof on both ends: the relay server accepts the request and
    // the listener starts listening on the relayed address.
    let relay_task = async {
        loop {
            if let SwarmEvent::Behaviour(RelayNodeEvent::Relay(
                relay::Event::ReservationReqAccepted { src_peer_id, .. },
            )) = relay_node.select_next_some().await
                && src_peer_id == listener_peer_id
            {
                break;
            }
        }
    };
    let listener_task = async {
        let mut accepted = false;
        let mut listening = false;
        while !(accepted && listening) {
            match listener.select_next_some().await {
                SwarmEvent::Behaviour(ClientNodeEvent::RelayClient(
                    relay::client::Event::ReservationReqAccepted {
                        relay_peer_id: peer,
                        ..
                    },
                )) if peer == relay_peer_id => accepted = true,
                SwarmEvent::NewListenAddr { address, .. } if address == relayed_addr => {
                    listening = true;
                }
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(relay_task, listener_task)
    })
    .await
    .expect("relay reservation was not granted within 30s");

    // The relay and the listener only need to keep making progress from here;
    // the assertions move to the dialer.
    tokio::spawn(relay_node.loop_on_next());
    tokio::spawn(listener.loop_on_next());

    dialer
        .dial(relayed_addr)
        .expect("dial the relayed circuit address");

    // The hole punch upgrades the circuit: DCUtR reports success for the
    // listener peer, and the very connection it reports was established to the
    // listener's own TCP multiaddr rather than through the relay.
    let listener_direct_addr = listener_addr.with(Protocol::P2p(listener_peer_id));
    let (direct_conn, punched_conn) = tokio::time::timeout(Duration::from_secs(30), async {
        let mut direct = None;
        let mut punched = None;
        loop {
            match dialer.select_next_some().await {
                SwarmEvent::ConnectionEstablished {
                    endpoint,
                    connection_id,
                    ..
                } if *endpoint.get_remote_address() == listener_direct_addr => {
                    direct = Some(connection_id);
                }
                SwarmEvent::Behaviour(ClientNodeEvent::Dcutr(dcutr::Event {
                    remote_peer_id,
                    result: Ok(connection_id),
                })) if remote_peer_id == listener_peer_id => punched = Some(connection_id),
                _ => {}
            }
            if let (Some(direct), Some(punched)) = (direct, punched) {
                break (direct, punched);
            }
        }
    })
    .await
    .expect("DCUtR hole punch did not complete within 30s");

    assert_eq!(direct_conn, punched_conn);
}
