use std::sync::Arc;

use libp2p_swarm::Swarm;
use libp2p_swarm_test::SwarmExt;
use tracing_subscriber;
use vertex_network_handshake::{HandshakeBehaviour, HandshakeConfig, HandshakeEvent};

#[tokio::test]
async fn test_concurrent_handshakes() {
    tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .init();

    const NETWORK_ID: u64 = 1;
    let cfg_1 = Arc::new(HandshakeConfig::<NETWORK_ID>::default());
    let cfg_2 = Arc::new(HandshakeConfig::<NETWORK_ID>::default());

    let mut swarm1 = Swarm::new_ephemeral(|_| HandshakeBehaviour::<NETWORK_ID>::new(cfg_1.clone()));
    let mut swarm2 = Swarm::new_ephemeral(|_| HandshakeBehaviour::<NETWORK_ID>::new(cfg_2.clone()));

    let result = tokio::spawn(async move {
        // Start listening on swarm1
        swarm1.listen().with_memory_addr_external().await;

        // Connect swarm2 to swarm1 (this will trigger first handshake)
        swarm2.connect(&mut swarm1).await;

        // Immediately try to start another handshake from swarm2
        // Get the peer ID of swarm1
        let peer_id = *swarm1.local_peer_id();

        // Extract the connection ID and handler from swarm2
        if let Some(connection) = swarm2.network_info().connections().next() {
            swarm2
                .behaviour_mut()
                .on_swarm_event(FromSwarm::ConnectionEstablished(
                    libp2p::swarm::ConnectionEstablished {
                        peer_id,
                        connection_id: *connection,
                        endpoint: &libp2p::core::ConnectedPoint::Dialer {
                            address: "/memory/0".parse().unwrap(),
                            role_override: libp2p::core::Endpoint::Dialer,
                        },
                        failed_addresses: &[],
                        other_established: 0,
                    },
                ));
        }

        // Drive both swarms and collect events
        let ([e1], [e2]): (
            [HandshakeEvent<NETWORK_ID>; 1],
            [HandshakeEvent<NETWORK_ID>; 1],
        ) = libp2p_swarm_test::drive(&mut swarm1, &mut swarm2).await;

        // We expect only one successful handshake, since the second one should be ignored
        match (&e1, &e2) {
            (HandshakeEvent::Completed(_), HandshakeEvent::Completed(_)) => {
                println!("First handshake completed successfully");
            }
            _ => panic!(
                "Expected Completed events for first handshake, got: {:?}",
                (e1, e2)
            ),
        }
    })
    .await;

    assert!(result.is_ok());
}
