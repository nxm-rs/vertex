//! Scripted peer behaviour over the simulated network.
//!
//! Scripted peers are real networked hosts: each runs a handshake-serving
//! swarm (or refuses to listen), so the node under test exercises its
//! production dial, handshake, and disconnect paths against them.

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{B256, keccak256};
use libp2p::{Multiaddr, PeerId, multiaddr::Protocol};
use rand::{SeedableRng, rngs::StdRng, seq::SliceRandom};
use vertex_swarm_api::SwarmSpec;
use vertex_swarm_identity::Identity;
use vertex_swarm_net_handshake::{HandshakeBehaviour, HandshakeEvent, NoAddresses};
use vertex_swarm_primitives::{Bin, Nonce, OverlayAddress, SwarmNodeType, compute_overlay};
use vertex_swarm_spec::Spec;

use crate::SimAuth;
use crate::host::{HostContext, host_keypair, host_signer};
use crate::world::{HostResult, SimWorld, listen_multiaddr};

/// Port every scripted peer listens on; hosts are distinguished by IP.
const SCRIPTED_PEER_PORT: u16 = 1634;

/// Idle timeout for scripted-peer swarms. Scripted peers exist to hold
/// topology connections across long virtual horizons, so the timeout must
/// outlive any scenario schedule.
const SCRIPTED_PEER_IDLE: Duration = Duration::from_secs(24 * 3600);

/// Scripted wire behaviour for a simulated peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerScript {
    /// Listens and completes every handshake for the whole run.
    Honest,
    /// Registered and resolvable but never listens: every dial to it fails.
    Unreachable,
    /// Completes handshakes, then cleanly closes every live connection each
    /// `hold` of virtual time; it keeps listening, so it stays re-dialable.
    AcceptThenDrop {
        /// Virtual time between clean connection drops.
        hold: Duration,
    },
    /// Honest on the wire; the adversarial trait is address-space
    /// clustering, set up by grinding the peer's overlay into a
    /// caller-chosen bin via a [`Placement`].
    Sybil,
}

/// Address-space placement: grind the peer's nonce until its overlay lands
/// exactly in `bin` relative to `anchor`.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    /// Overlay the bin is measured against (usually the node under test).
    pub anchor: OverlayAddress,
    /// Exact proximity bin the ground overlay must occupy.
    pub bin: Bin,
}

/// A registered scripted peer and the coordinates a test needs to reach it.
#[derive(Clone, Debug)]
pub struct ScriptedPeer {
    /// Host name in the world.
    pub name: String,
    /// Wire behaviour the host runs.
    pub script: PeerScript,
    /// Node type the peer advertises in its handshake.
    pub node_type: SwarmNodeType,
    /// The peer's libp2p identity.
    pub peer_id: PeerId,
    /// The peer's overlay address (placement-ground when placed).
    pub overlay: OverlayAddress,
    /// Dialable multiaddr including the `/p2p/` component.
    pub multiaddr: Multiaddr,
}

/// A scripted population registered into a [`SimWorld`].
///
/// Peers register as restartable hosts, so churn can bounce them and each
/// incarnation keeps its identity. All scripted traffic uses the
/// entropy-free plaintext stack so runs stay reproducible from the seed.
pub struct Scenario {
    spec: Arc<Spec>,
    seed: u64,
    rng: StdRng,
    peers: Vec<ScriptedPeer>,
}

impl Scenario {
    /// A scenario over `world`, deriving its churn RNG from the world seed.
    pub fn new(world: &SimWorld, spec: Arc<Spec>) -> Self {
        Self {
            spec,
            seed: world.seed(),
            rng: StdRng::seed_from_u64(world.seed()),
            peers: Vec::new(),
        }
    }

    /// Register a scripted peer as a restartable host, grinding its overlay
    /// into place when `placement` is given.
    pub fn add_peer(
        &mut self,
        world: &mut SimWorld,
        name: &str,
        node_type: SwarmNodeType,
        script: PeerScript,
        placement: Option<Placement>,
    ) -> &ScriptedPeer {
        let nonce = match placement {
            Some(placement) => placement_nonce(self.seed, name, &self.spec, placement),
            None => Nonce::new(placement_digest(self.seed, name).0),
        };
        let overlay = compute_overlay(
            &host_signer(self.seed, name).address(),
            self.spec.network_id(),
            &nonce,
        );
        let peer_id = host_keypair(self.seed, name).public().to_peer_id();

        let spec = Arc::clone(&self.spec);
        world.host(name, move |ctx| {
            run_script(ctx, Arc::clone(&spec), node_type, nonce, script)
        });
        let multiaddr = world
            .multiaddr_of(name, SCRIPTED_PEER_PORT)
            .with(Protocol::P2p(peer_id));

        self.peers.push(ScriptedPeer {
            name: name.to_owned(),
            script,
            node_type,
            peer_id,
            overlay,
            multiaddr,
        });
        #[allow(clippy::expect_used)]
        self.peers.last().expect("peer pushed above")
    }

    /// The registered population.
    pub fn peers(&self) -> &[ScriptedPeer] {
        &self.peers
    }

    /// Every scripted peer's dialable multiaddr, for a node's bootnode list.
    pub fn bootnodes(&self) -> Vec<Multiaddr> {
        self.peers.iter().map(|p| p.multiaddr.clone()).collect()
    }

    /// Bounce a seeded `fraction` of the running scripted peers, dropping
    /// their connections and network state; each restarts with the same
    /// identity. Returns the bounced host names.
    pub fn churn_burst(&mut self, world: &mut SimWorld, fraction: f64) -> Vec<String> {
        let mut names: Vec<String> = self
            .peers
            .iter()
            .map(|p| p.name.clone())
            .filter(|name| world.is_host_running(name))
            .collect();
        // Registration order is deterministic, but sort before the seeded
        // shuffle so the selection never depends on it.
        names.sort();
        names.shuffle(&mut self.rng);
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        #[allow(clippy::cast_sign_loss)]
        let take = ((names.len() as f64) * fraction) as usize;
        let victims: Vec<String> = names.into_iter().take(take).collect();
        for name in &victims {
            world.bounce(name);
        }
        victims
    }
}

/// Grind a nonce so the host's overlay lands exactly in the placement bin.
///
/// The expected cost doubles per bin, so scenarios place peers in shallow
/// bins. Deterministic in (seed, host, spec, placement).
pub fn placement_nonce(world_seed: u64, host: &str, spec: &Spec, placement: Placement) -> Nonce {
    let address = host_signer(world_seed, host).address();
    let mut candidate = placement_digest(world_seed, host);
    loop {
        let nonce = Nonce::new(candidate.0);
        let overlay = compute_overlay(&address, spec.network_id(), &nonce);
        if placement.anchor.proximity(&overlay).get() == placement.bin.get() {
            return nonce;
        }
        candidate = keccak256(candidate);
    }
}

fn placement_digest(world_seed: u64, host: &str) -> B256 {
    const DOMAIN: &str = "vertex-sim/placement/";
    let mut input = Vec::with_capacity(DOMAIN.len() + 8 + host.len());
    input.extend_from_slice(DOMAIN.as_bytes());
    input.extend_from_slice(&world_seed.to_be_bytes());
    input.extend_from_slice(host.as_bytes());
    keccak256(&input)
}

/// Canonical handshake-event normalizer for scripted-peer traces.
pub fn handshake_summary(event: &HandshakeEvent) -> String {
    match event {
        HandshakeEvent::Completed { peer_id, info, .. } => format!(
            "handshake-completed peer={peer_id} overlay={}",
            info.swarm_peer.overlay()
        ),
        HandshakeEvent::Failed { peer_id, error, .. } => {
            format!("handshake-failed peer={peer_id} error={error}")
        }
    }
}

async fn run_script(
    ctx: HostContext,
    spec: Arc<Spec>,
    node_type: SwarmNodeType,
    nonce: Nonce,
    script: PeerScript,
) -> HostResult {
    if matches!(script, PeerScript::Unreachable) {
        futures::future::pending::<()>().await;
        return Ok(());
    }

    let identity = Arc::new(Identity::new(ctx.signer(), nonce, spec, node_type));
    let swarm = ctx.swarm_with_idle(SimAuth::Plaintext, SCRIPTED_PEER_IDLE, move |_keypair| {
        HandshakeBehaviour::new(identity, Arc::new(NoAddresses), "sim")
    });
    let mut traced = ctx.traced(swarm, handshake_summary);
    traced
        .swarm_mut()
        .listen_on(listen_multiaddr(SCRIPTED_PEER_PORT))?;

    match script {
        PeerScript::AcceptThenDrop { hold } => loop {
            traced.drive_for(hold).await;
            let connected: Vec<PeerId> = traced.swarm().connected_peers().copied().collect();
            for peer in connected {
                let _ = traced.swarm_mut().disconnect_peer_id(peer);
            }
        },
        _ => loop {
            traced.drive_for(Duration::from_secs(3600)).await;
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_test_utils::test_spec;

    #[test]
    fn placement_lands_the_exact_bin() {
        let spec = test_spec();
        let anchor = OverlayAddress::from([0xAA; 32]);
        for bin in 0..4u8 {
            let bin = Bin::new(bin).unwrap_or(Bin::MAX);
            let placement = Placement { anchor, bin };
            let nonce = placement_nonce(7, "peer", &spec, placement);
            let overlay =
                compute_overlay(&host_signer(7, "peer").address(), spec.network_id(), &nonce);
            assert_eq!(anchor.proximity(&overlay).get(), bin.get());
            // Deterministic: the same inputs grind the same nonce.
            assert_eq!(nonce, placement_nonce(7, "peer", &spec, placement));
        }
    }

    #[test]
    fn churn_selection_is_seeded() {
        fn victims(seed: u64) -> Vec<String> {
            let mut world = SimWorld::builder()
                .seed(seed)
                .duration(Duration::from_secs(10))
                .build();
            let mut scenario = Scenario::new(&world, test_spec());
            for idx in 0..10 {
                scenario.add_peer(
                    &mut world,
                    &format!("peer-{idx}"),
                    SwarmNodeType::Storer,
                    PeerScript::Honest,
                    None,
                );
            }
            // A pending client keeps the world stepping while the hosts start.
            world.client("driver", |_ctx| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            });
            #[allow(clippy::expect_used)]
            world
                .run_for(Duration::from_millis(100))
                .expect("world advances");
            scenario.churn_burst(&mut world, 0.4)
        }

        let first = victims(5);
        assert_eq!(first.len(), 4, "40% of ten peers is four victims");
        assert_eq!(first, victims(5), "same seed selects the same victims");
        assert_ne!(first, victims(6), "a different seed reorders the burst");
    }
}
