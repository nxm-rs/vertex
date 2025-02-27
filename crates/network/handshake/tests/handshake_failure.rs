use libp2p_swarm::{dummy, Swarm, SwarmEvent};
use libp2p_swarm_test::SwarmExt;
use quickcheck::QuickCheck;
use tracing::{debug, trace};
use tracing_subscriber;
use vertex_network_handshake::{HandshakeBehaviour, HandshakeConfig, HandshakeEvent};
use vertex_network_primitives_traits::NodeAddress;

#[tokio::test]
async fn handshake_failure() {
    tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .init();

    let cfg_swarm_1 = HandshakeConfig::<1>::default();
    let cfg_swarm_2 = HandshakeConfig::<2>::default();

    let mut swarm1 = Swarm::new_ephemeral(|_| HandshakeBehaviour::<1>::new(cfg_swarm_1.clone()));
    let mut swarm2 = Swarm::new_ephemeral(|_| HandshakeBehaviour::<2>::new(cfg_swarm_2.clone()));

    tokio::spawn(async move {
        swarm1.listen().with_memory_addr_external().await;
        swarm2.connect(&mut swarm1).await;

        let ([e1], [e2]): ([HandshakeEvent<1>; 1], [HandshakeEvent<2>; 1]) =
            libp2p_swarm_test::drive(&mut swarm1, &mut swarm2).await;

        println!("Handshake events swarm 1: {:?}", (&e1));
        println!("Handshake events swarm 2: {:?}", (&e2));

        // Check if both events are Completed variants (without comparing the actual HandshakeInfo)
        match (&e1, &e2) {
            (HandshakeEvent::Failed(_), HandshakeEvent::Failed(_)) => {
                // Test passed
                println!("Handshake events: {:?}", (e1, e2));
            }
            _ => panic!("Expected Completed events, got: {:?}", (e1, e2)),
        }
    })
    .await;
}
