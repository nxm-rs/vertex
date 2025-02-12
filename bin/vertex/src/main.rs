use futures::StreamExt;
use libp2p::{swarm::NetworkBehaviour, Multiaddr};
use tracing::{info, warn};

mod handshake;
use handshake::*;

mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}
use proto::*;

#[derive(NetworkBehaviour)]
pub struct SwarmBehaviour<const N: u64> {
    handshake: HandshakeBehaviour<N>,
    identify: libp2p::identify::Behaviour,
    ping: libp2p::ping::Behaviour,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialise the tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("vertex=debug".parse()?), // .add_directive("trace".parse()?),
        )
        .with_target(true)
        // .with_thread_ids(true)
        // .with_thread_names(true)
        // .with_file(true)
        // .with_line_number(true)
        // .with_timer(tracing_subscriber::fmt::time::ChronoUtc::rfc3339())
        .init();

    info!("Starting vertex node");

    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?
        .with_dns_config(
            libp2p::dns::ResolverConfig::default(),
            libp2p::dns::ResolverOpts::default(),
        )
        .with_behaviour(|key| SwarmBehaviour {
            handshake: HandshakeBehaviour::<1>::new(HandshakeConfig::default()),
            identify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                "/ipfs/id/1.0.0".to_string(),
                key.public(),
            )),
            ping: libp2p::ping::Behaviour::new(libp2p::ping::Config::default()),
        })?
        .build();

    // Listen on localhost port 1634
    swarm.listen_on("/ip4/127.0.0.1/tcp/2634".parse()?)?;

    // If a peer address is provided as an argument, dial it
    if let Some(addr) = std::env::args().nth(1) {
        // Parse the multiaddr
        let remote: Multiaddr = addr.parse()?;
        info!("Dialing peer at {}", remote);
        swarm.dial(remote)?;
    }

    // Event loop
    loop {
        match swarm.select_next_some().await {
            libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on {}", address);
            }
            libp2p::swarm::SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                info!("Connection established with {}", peer_id);
            }
            libp2p::swarm::SwarmEvent::Behaviour(event) => {
                info!("Handshake event: {:?}", event);
            }
            event => {
                info!("Unhandled event: {:?}", event);
            }
        }
    }
}
