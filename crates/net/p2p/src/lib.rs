use std::{default, io, time::Duration, vec};

use alloy_primitives::FixedBytes;
use alloy_signer::Signature;
use alloy_signer_wallet::LocalWallet;
use eyre::Result;
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use handshake::swarm::handshake::BzzAddress;
use libp2p::{
    autonat, identify, identity,
    multiaddr::Protocol,
    noise, ping,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, Stream, StreamProtocol,
};
use libp2p_stream as stream;
use prost::{bytes::buf, Message};
use rand::RngCore;
use beers_primitives::NodeAddress;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

pub async fn run() -> eyre::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::DEBUG.into())
                .from_env()?,
        )
        .init();

    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_dns()?
        .with_behaviour(|key| Behaviour::new(key.public()))?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(10)))
        .build();

    swarm.listen_on("/ip4/0.0.0.0/tcp/2634".parse()?)?;

    // let peer = "/ip4/139.84.229.70/tcp/1634/p2p/QmRa6rSrUWJ7s68MNmV94bo2KAa9pYcp6YbFLMHZ3r7n2M".parse::<Multiaddr>()?;
    // let peer = "/ip4/142.132.214.211/tcp/3400/p2p/QmXr7inNU2nekBxoYkPwos39ipiSZNizKPQSYkVchrXt3M".parse::<Multiaddr>()?;
    let peer = "/ip4/127.0.0.1/tcp/1634/p2p/QmcmeGcwx3YTBmBTQ5MdGh1SJfCpue7v5fqVuE8iUv9JYb"
        .parse::<Multiaddr>()?;

    swarm.dial(peer.clone())?;

    tokio::spawn(connection_handler(
        peer.clone(),
        swarm.behaviour().handshake.new_control(),
        "/ip4/0.0.0.0/tcp/2634".parse()?,
    ));

    let mut incoming_streams = swarm
        .behaviour()
        .handshake
        .new_control()
        .accept(StreamProtocol::new("/swarm/handshake/11.0.0/handshake"))
        .unwrap();

    tokio::spawn(async move {
        while let Some((peer, stream)) = incoming_streams.next().await {
            match echo(stream).await {
                Ok(_) => println!("Echoed to {peer:?}"),
                Err(e) => eprintln!("Error echoing to {peer:?}: {e}"),
            }
        }
    });

    // Poll the swarm to make progress.
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => println!("Listening on {address:?}"),
            SwarmEvent::Behaviour(BehaviourEvent::Identify(event)) => match event {
                identify::Event::Received { peer_id, info } => {
                    println!("Identified {peer_id:?} as {info:?}");
                }
                identify::Event::Sent { peer_id, .. } => {
                    println!("Sent identify info to {peer_id:?}");
                }
                _ => {}
            },
            // SwarmEvent::Behaviour(BehaviourEvent::AutoNat(event)) => println!("AutoNAT: {event:?}"),
            SwarmEvent::Behaviour(BehaviourEvent::Ping(event)) => match event {
                ping::Event {
                    connection: _,
                    peer,
                    result,
                } => println!("Ping from {peer:?} in {result:?}"),
            },
            _ => {}
        }
    }
}

const HANDSHAKE_PROTOCOL: StreamProtocol = StreamProtocol::new("/swarm/handshake/11.0.0/handshake");

/// A very simple, `async fn`-based connection handler for our custom echo protocol.
async fn connection_handler(
    ma: Multiaddr,
    mut control: stream::Control,
    own_underlay: Multiaddr,
) -> Result<()> {
    let Protocol::P2p(peer_id) = ma
        .clone()
        .pop()
        .expect("peer address must have a p2p component")
    else {
        return Err(eyre::Error::msg("peer address must have a p2p component"));
    };
    let stream = match control.open_stream(peer_id, HANDSHAKE_PROTOCOL).await {
        Ok(stream) => stream,
        Err(error @ stream::OpenStreamError::UnsupportedProtocol(_)) => {
            tracing::info!(%peer_id, %error);
            return Err(error.into());
        }
        Err(error) => {
            // Other errors may be temporary.
            // In production, something like an exponential backoff / circuit-breaker may be more appropriate.
            tracing::debug!(%peer_id, %error);
            return Err(error.into());
        }
    };

    if let Err(e) = send(stream, ma.clone(), own_underlay).await {
        tracing::warn!(%peer_id, "Echo protocol failed: {e}");
        return Err(e.into());
    }

    tracing::info!(%peer_id, "Echo complete!");
    Ok(())
}

async fn send(mut stream: Stream, ma: Multiaddr, own_underlay: Multiaddr) -> io::Result<()> {
    let syn = handshake::swarm::handshake::Syn {
        observed_underlay: ma.to_vec(),
    };

    // let mut buf = vec![0; syn.encode_length_delimited_to_vec()];
    // syn.encode(&mut buf).unwrap();
    // println!("{:?}", hex::encode(buf.as_slice()));
    stream
        .write_all(&syn.encode_length_delimited_to_vec())
        .await?;
    stream.flush().await?;

    // wait for syn-ack

    let mut b = vec![0u8; 4096];
    let recv_amt = stream.read(b.as_mut_slice()).await?;
    println!("{:?}", recv_amt);
    let syn_ack =
        handshake::swarm::handshake::SynAck::decode_length_delimited(b.as_slice()).unwrap();

    println!("{:?}", syn_ack);

    let wallet = LocalWallet::random();
    let nonce = FixedBytes::<32>::default();
    let (address, signature) = NodeAddress::new(wallet, 1, nonce.clone(), own_underlay).await;

    let ack = handshake::swarm::handshake::Ack {
        address: Some(BzzAddress {
            underlay: address.underlay().to_vec(),
            signature: signature.as_bytes().to_vec(),
            overlay: address.overlay().to_vec(),
        }),
        network_id: 1,
        full_node: false,
        nonce: nonce.to_vec(),
        welcome_message: "hello from rust!!!".to_string(),
    };

    stream
        .write_all(&ack.encode_length_delimited_to_vec())
        .await?;
    stream.flush().await?;

    stream.close().await?;

    Ok(())
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    identify: identify::Behaviour,
    // auto_nat: autonat::Behaviour,
    ping: ping::Behaviour,
    handshake: stream::Behaviour,
}

impl Behaviour {
    fn new(key: identity::PublicKey) -> Self {
        Self {
            identify: identify::Behaviour::new(identify::Config::new(
                "/ipfs/id/1.0.0".to_string(),
                key.clone(),
            )),
            // auto_nat: autonat::Behaviour::new(
            //     key.to_peer_id(),
            //     autonat::Config {
            //         retry_interval: Duration::from_secs(10),
            //         refresh_interval: Duration::from_secs(30),
            //         boot_delay: Duration::from_secs(5),
            //         throttle_server_period: Duration::ZERO,
            //         only_global_ips: false,
            //         ..Default::default()
            //     },
            // ),
            ping: ping::Behaviour::default(),
            handshake: stream::Behaviour::new(),
        }
    }
}

async fn echo(mut stream: Stream) -> io::Result<usize> {
    let mut total = 0;

    let mut buf = [0u8; 100];

    loop {
        let read = stream.read(&mut buf).await?;
        if read == 0 {
            return Ok(total);
        }

        total += read;
        stream.write_all(&buf[..read]).await?;
    }
}
