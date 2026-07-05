//! Per-host context: seeded identities, swarm construction, and traced
//! driving.
//!
//! Every identity derives from the world seed plus the host name, so a host
//! keeps its identity across a restart and the plaintext path draws no OS
//! entropy.

use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use alloy_primitives::{B256, keccak256};
use alloy_signer_local::PrivateKeySigner;
use futures::StreamExt;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, Swarm, identity::Keypair, multiaddr::Protocol};
use vertex_swarm_identity::Identity;
use vertex_swarm_primitives::{Nonce, SwarmNodeType};
use vertex_swarm_spec::Spec;

use crate::drive::DrivableSwarm;
use crate::trace::{SimTrace, normalized_event};
use crate::{SimAuth, stack};

fn digest(world_seed: u64, host: &str, domain: &str) -> B256 {
    let mut input = Vec::with_capacity(domain.len() + 8 + host.len());
    input.extend_from_slice(domain.as_bytes());
    input.extend_from_slice(&world_seed.to_be_bytes());
    input.extend_from_slice(host.as_bytes());
    keccak256(&input)
}

/// Deterministic libp2p keypair for a host, derived from the world seed and
/// the host name.
#[allow(clippy::expect_used)]
pub fn host_keypair(world_seed: u64, host: &str) -> Keypair {
    let secret = libp2p::identity::ed25519::SecretKey::try_from_bytes(
        digest(world_seed, host, "vertex-sim/libp2p/").0,
    )
    .expect("32 bytes is a valid ed25519 secret key");
    Keypair::from(libp2p::identity::ed25519::Keypair::from(secret))
}

/// Deterministic overlay signer for a host.
///
/// The vanishingly rare digest that is invalid as a secp256k1 scalar is
/// rehashed, so the derivation is total.
pub fn host_signer(world_seed: u64, host: &str) -> PrivateKeySigner {
    let mut candidate = digest(world_seed, host, "vertex-sim/signer/");
    loop {
        if let Ok(signer) = PrivateKeySigner::from_bytes(&candidate) {
            return signer;
        }
        candidate = keccak256(candidate);
    }
}

/// Deterministic nonce for a host.
pub fn host_nonce(world_seed: u64, host: &str) -> Nonce {
    Nonce::new(digest(world_seed, host, "vertex-sim/nonce/").0)
}

/// Handle a host future receives from the world: its name, the world seed,
/// derived identity material, and the shared trace.
///
/// Clone-cheap; a restartable host gets a fresh clone per incarnation, and
/// because everything derives from (seed, name) the identity survives the
/// restart.
#[derive(Debug, Clone)]
pub struct HostContext {
    seed: u64,
    name: String,
    trace: SimTrace,
}

impl HostContext {
    pub(crate) fn new(seed: u64, name: &str, trace: SimTrace) -> Self {
        Self {
            seed,
            name: name.to_owned(),
            trace,
        }
    }

    /// The host name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The world seed.
    pub fn world_seed(&self) -> u64 {
        self.seed
    }

    /// Virtual time elapsed in the simulation (zero outside one).
    pub fn elapsed(&self) -> Duration {
        turmoil::sim_elapsed().unwrap_or_default()
    }

    /// This host's libp2p keypair.
    pub fn keypair(&self) -> Keypair {
        host_keypair(self.seed, &self.name)
    }

    /// This host's PeerId.
    pub fn peer_id(&self) -> PeerId {
        self.keypair().public().to_peer_id()
    }

    /// This host's overlay signer.
    pub fn signer(&self) -> PrivateKeySigner {
        host_signer(self.seed, &self.name)
    }

    /// This host's nonce.
    pub fn nonce(&self) -> Nonce {
        host_nonce(self.seed, &self.name)
    }

    /// A persistent vertex identity for this host on `spec`.
    pub fn identity(&self, spec: Arc<Spec>, node_type: SwarmNodeType) -> Identity {
        Identity::new(self.signer(), self.nonce(), spec, node_type)
    }

    /// Build a swarm over the simulated network with this host's keypair.
    pub fn swarm<B: NetworkBehaviour>(
        &self,
        auth: SimAuth,
        build: impl FnOnce(&Keypair) -> B,
    ) -> Swarm<B> {
        let keypair = self.keypair();
        let behaviour = build(&keypair);
        let peer_id = keypair.public().to_peer_id();
        Swarm::new(
            stack(auth, &keypair),
            behaviour,
            peer_id,
            libp2p::swarm::Config::with_tokio_executor()
                .with_idle_connection_timeout(Duration::from_secs(60)),
        )
    }

    /// Wrap `swarm` so every observed event lands in the world trace,
    /// normalizing behaviour events through `behaviour`.
    pub fn traced<B: NetworkBehaviour>(
        &self,
        swarm: Swarm<B>,
        behaviour: impl Fn(&B::ToSwarm) -> String + 'static,
    ) -> TracedSwarm<B> {
        TracedSwarm {
            swarm,
            host: self.name.clone(),
            trace: self.trace.clone(),
            behaviour: Box::new(behaviour),
        }
    }

    /// Append a line to the world trace under this host's name.
    pub fn record(&self, line: impl Into<String>) {
        self.trace.record(&self.name, line.into());
    }

    /// Resolve another host's name to a dialable `/ip6|ip4/../tcp/<port>`
    /// multiaddr. Only valid inside a running simulation.
    pub fn lookup_multiaddr(&self, host: &str, port: u16) -> Multiaddr {
        Multiaddr::empty()
            .with(turmoil::lookup(host).into())
            .with(Protocol::Tcp(port))
    }
}

/// Behaviour-event normalizer a [`TracedSwarm`] records through.
type BehaviourFormatter<E> = Box<dyn Fn(&E) -> String>;

/// A swarm whose events are recorded into the world trace as they are
/// observed.
///
/// Events polled through [`Self::swarm_mut`] bypass the trace; drive through
/// the methods here (or [`DrivableSwarm`]) to keep the record complete.
pub struct TracedSwarm<B: NetworkBehaviour> {
    swarm: Swarm<B>,
    host: String,
    trace: SimTrace,
    behaviour: BehaviourFormatter<B::ToSwarm>,
}

impl<B: NetworkBehaviour> TracedSwarm<B> {
    /// The wrapped swarm.
    pub fn swarm(&self) -> &Swarm<B> {
        &self.swarm
    }

    /// Mutable access to the wrapped swarm (commands, listen, dial).
    pub fn swarm_mut(&mut self) -> &mut Swarm<B> {
        &mut self.swarm
    }

    fn record(&self, event: &SwarmEvent<B::ToSwarm>) {
        self.trace
            .record(&self.host, normalized_event(event, &self.behaviour));
    }

    /// Next swarm event, recorded before it is returned.
    pub async fn next_event(&mut self) -> SwarmEvent<B::ToSwarm> {
        let event = self.swarm.select_next_some().await;
        self.record(&event);
        event
    }

    /// Drive until `predicate` holds for an event or `timeout` of virtual
    /// time elapses, returning whether it held.
    ///
    /// The select is biased (deadline first) so branch order never draws from
    /// tokio's unseeded RNG.
    pub async fn drive_until(
        &mut self,
        timeout: Duration,
        mut predicate: impl FnMut(&SwarmEvent<B::ToSwarm>) -> bool,
    ) -> bool {
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                biased;
                () = &mut deadline => return false,
                event = self.swarm.select_next_some() => {
                    self.record(&event);
                    if predicate(&event) {
                        return true;
                    }
                }
            }
        }
    }

    /// Drive for `duration` of virtual time, recording every event.
    pub async fn drive_for(&mut self, duration: Duration) {
        let _ = self.drive_until(duration, |_| false).await;
    }
}

impl<B: NetworkBehaviour> DrivableSwarm for TracedSwarm<B> {
    fn poll_drive(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        match self.swarm.poll_next_unpin(cx) {
            Poll::Ready(Some(event)) => {
                self.record(&event);
                Poll::Ready(())
            }
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_stable_and_distinct() {
        assert_eq!(
            host_keypair(7, "a").public().to_peer_id(),
            host_keypair(7, "a").public().to_peer_id()
        );
        assert_ne!(
            host_keypair(7, "a").public().to_peer_id(),
            host_keypair(7, "b").public().to_peer_id()
        );
        assert_ne!(
            host_keypair(7, "a").public().to_peer_id(),
            host_keypair(8, "a").public().to_peer_id()
        );

        assert_eq!(host_signer(7, "a").address(), host_signer(7, "a").address());
        assert_ne!(host_signer(7, "a").address(), host_signer(7, "b").address());

        assert_eq!(host_nonce(7, "a"), host_nonce(7, "a"));
        assert_ne!(host_nonce(7, "a"), host_nonce(8, "a"));
    }

    #[test]
    fn streams_do_not_alias() {
        // The keypair, signer, and nonce digests are domain-separated, so one
        // host's streams never coincide.
        let keypair_digest = digest(7, "a", "vertex-sim/libp2p/");
        let signer_digest = digest(7, "a", "vertex-sim/signer/");
        let nonce_digest = digest(7, "a", "vertex-sim/nonce/");
        assert_ne!(keypair_digest, signer_digest);
        assert_ne!(signer_digest, nonce_digest);
    }
}
