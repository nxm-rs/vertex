use std::sync::Arc;

use libp2p_swarm::Swarm;
use libp2p_swarm_test::SwarmExt;
use tracing_subscriber;
use vertex_network_handshake::{HandshakeBehaviour, HandshakeConfig, HandshakeEvent};

#[tokio::test]
async fn handshake_success() {
    tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .init();

    let cfg_swarm_1 = Arc::new(HandshakeConfig::<1>::default());
    let cfg_swarm_2 = Arc::new(HandshakeConfig::<1>::default());

    let mut swarm1 = Swarm::new_ephemeral(|_| HandshakeBehaviour::<1>::new(cfg_swarm_1.clone()));
    let mut swarm2 = Swarm::new_ephemeral(|_| HandshakeBehaviour::<1>::new(cfg_swarm_2.clone()));

    let result = tokio::spawn(async move {
        swarm1.listen().with_memory_addr_external().await;
        swarm2.connect(&mut swarm1).await;

        let ([e1], [e2]): ([HandshakeEvent<1>; 1], [HandshakeEvent<1>; 1]) =
            libp2p_swarm_test::drive(&mut swarm1, &mut swarm2).await;

        // Check if both events are Completed variants (without comparing the actual HandshakeInfo)
        match (&e1, &e2) {
            (HandshakeEvent::Completed(_), HandshakeEvent::Completed(_)) => {}
            _ => panic!("Expected Completed events, got: {:?}", (e1, e2)),
        }
    })
    .await;

    assert!(result.is_ok())
}
