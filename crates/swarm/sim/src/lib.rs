//! Deterministic swarm simulation support.
//!
//! Dev-only workspace member: it must never appear in a shipped crate's
//! dependency cone. It provides a libp2p transport over turmoil's simulated
//! network plus a [`SimWorld`] that runs real vertex swarms under a seeded,
//! virtual-time scheduler with the same drive vocabulary as the seeded
//! behaviour harness.

use libp2p::{
    PeerId, Transport as _,
    core::{muxing::StreamMuxerBox, transport::Boxed, upgrade::Version},
    identity::Keypair,
    noise, plaintext, yamux,
};

mod drive;
mod host;
mod node;
mod probe;
mod scenario;
mod trace;
mod transport;
mod world;

pub use drive::{DrivableSwarm, drive};
pub use host::{HostContext, TracedSwarm, host_keypair, host_nonce, host_signer};
pub use node::{SimNetworkConfig, transport_override};
pub use probe::Probe;
pub use scenario::{
    Fault, FaultSchedule, Invariants, PeerScript, Placement, Scenario, ScriptedPeer,
    handshake_summary, placement_nonce, slot_of,
};
pub use trace::{SimTrace, TraceEntry, normalized_event};
pub use transport::{TurmoilStream, TurmoilTransport};
pub use world::{HostResult, SimError, SimWorld, SimWorldBuilder, listen_multiaddr};

/// Authentication upgrade a simulated stack negotiates.
///
/// Plaintext is the default because it draws no OS entropy, so runs stay
/// reproducible from the sim seed alone; noise matches the production native
/// stack at the cost of per-run ephemeral key bytes on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SimAuth {
    /// Entropy-free plaintext authentication.
    #[default]
    Plaintext,
    /// Production-shaped noise authentication.
    Noise,
}

/// The turmoil stack for `auth`, authenticated and muxed with yamux.
#[allow(clippy::expect_used)]
pub fn stack(auth: SimAuth, keypair: &Keypair) -> Boxed<(PeerId, StreamMuxerBox)> {
    match auth {
        SimAuth::Plaintext => plaintext_stack(keypair),
        SimAuth::Noise => noise_stack(keypair).expect("noise config from a valid keypair"),
    }
}

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
