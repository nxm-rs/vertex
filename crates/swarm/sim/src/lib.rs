//! Deterministic swarm simulation support.
//!
//! Dev-only workspace member: it must never appear in a shipped crate's
//! dependency cone. It provides a libp2p transport over turmoil's simulated
//! network so real vertex swarms run under a seeded, virtual-time scheduler.

use libp2p::{
    PeerId, Transport as _,
    core::{muxing::StreamMuxerBox, transport::Boxed, upgrade::Version},
    identity::Keypair,
    noise, plaintext, yamux,
};

mod transport;
pub use transport::{TurmoilStream, TurmoilTransport};

/// Turmoil transport authenticated with plaintext and muxed with yamux.
///
/// Plaintext keeps the sim free of OS entropy; use it on any path that must
/// be reproducible from the sim seed alone.
pub fn plaintext_stack(keypair: &Keypair) -> Boxed<(PeerId, StreamMuxerBox)> {
    TurmoilTransport::default()
        .upgrade(Version::V1)
        .authenticate(plaintext::Config::new(keypair))
        .multiplex(yamux::Config::default())
        .boxed()
}

/// Turmoil transport authenticated with noise and muxed with yamux, matching
/// the production native stack.
///
/// Noise draws ephemeral X25519 keys from the OS RNG, so wire bytes differ
/// between runs even under a fixed sim seed.
pub fn noise_stack(keypair: &Keypair) -> Result<Boxed<(PeerId, StreamMuxerBox)>, noise::Error> {
    Ok(TurmoilTransport::default()
        .upgrade(Version::V1)
        .authenticate(noise::Config::new(keypair)?)
        .multiplex(yamux::Config::default())
        .boxed())
}
