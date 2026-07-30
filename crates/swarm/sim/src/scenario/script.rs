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
use vertex_swarm_net_handshake::{AddressProvider, HandshakeBehaviour, HandshakeEvent};
use vertex_swarm_peer::{SwarmPeer, Timestamp};
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Accepts the transport but serves its handshake under a different
    /// network id, so the dialer's handshake validation fails after the
    /// connection is established.
    HandshakeFail,
    /// Actively dials `target` and holds the connection (an inbound peer
    /// from the target's perspective), re-dialling whenever it drops.
    DialAndHold {
        /// Multiaddr the peer keeps a connection to.
        target: Multiaddr,
    },
}

/// Address-space placement: grind the peer's nonce until its overlay lands
/// exactly in `bin` relative to `anchor`, optionally within one sub-prefix
/// slot of that bin.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    /// Overlay the bin is measured against (usually the node under test).
    pub anchor: OverlayAddress,
    /// Exact proximity bin the ground overlay must occupy.
    pub bin: Bin,
    /// Exact sub-prefix slot within the bin, when slot-aware selection is
    /// under test. `None` leaves the slot to the grind.
    pub slot: Option<u8>,
}

impl Placement {
    /// Placement in `bin` relative to `anchor`, any slot.
    pub fn new(anchor: OverlayAddress, bin: Bin) -> Self {
        Self {
            anchor,
            bin,
            slot: None,
        }
    }

    /// Pin the placement to one sub-prefix slot of the bin.
    pub fn in_slot(mut self, slot: u8) -> Self {
        self.slot = Some(slot);
        self
    }
}

/// Suffix-bit width of a sub-prefix slot key, mirroring the topology's
/// balanced-bin slot definition (the bits right after the bin's differing
/// bit). Scenario assertions read slot coverage from the production
/// readiness snapshot, so a drift in the production width surfaces as a
/// failed coverage assertion, not a silently weakened test.
const SLOT_BITS: u8 = 4;

/// The sub-prefix slot `overlay` occupies within `bin`.
pub fn slot_of(overlay: &OverlayAddress, bin: Bin) -> u8 {
    let bytes = overlay.as_bytes();
    let mut slot = 0u8;
    for i in 0..SLOT_BITS {
        let pos = bin.get() as usize + 1 + i as usize;
        let bit = bytes
            .get(pos / 8)
            .map_or(0, |byte| (byte >> (7 - (pos % 8))) & 1);
        slot = (slot << 1) | bit;
    }
    slot
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
    /// The peer's overlay nonce (placement-ground when placed).
    pub nonce: Nonce,
    /// Dialable multiaddr including the `/p2p/` component.
    pub multiaddr: Multiaddr,
}

impl ScriptedPeer {
    /// The peer's name-based multiaddr, resolved through the simulated name
    /// table at dial time.
    ///
    /// This is the address scripted peers advertise and gossip records
    /// carry: it has no IP literal, so the production address-scope filters
    /// admit it without consulting the real host's interface state.
    pub fn dns_multiaddr(&self) -> Multiaddr {
        dns_multiaddr(&self.name, self.peer_id)
    }
}

fn dns_multiaddr(name: &str, peer_id: PeerId) -> Multiaddr {
    Multiaddr::empty()
        .with(Protocol::Dns6(name.to_owned().into()))
        .with(Protocol::Tcp(SCRIPTED_PEER_PORT))
        .with(Protocol::P2p(peer_id))
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
    /// Strictly increasing timestamp source for signed gossip records, so a
    /// re-gossiped record is never rejected as stale.
    record_clock: i64,
}

impl Scenario {
    /// A scenario over `world`, deriving its churn RNG from the world seed.
    pub fn new(world: &SimWorld, spec: Arc<Spec>) -> Self {
        Self {
            spec,
            seed: world.seed(),
            rng: StdRng::seed_from_u64(world.seed()),
            peers: Vec::new(),
            record_clock: 0,
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
        let host_script = script.clone();
        let advertised = dns_multiaddr(name, peer_id);
        world.host(name, move |ctx| {
            run_script(
                ctx,
                Arc::clone(&spec),
                node_type,
                nonce,
                host_script.clone(),
                advertised.clone(),
            )
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
            nonce,
            multiaddr,
        });
        #[allow(clippy::expect_used)]
        self.peers.last().expect("peer pushed above")
    }

    /// The registered population.
    pub fn peers(&self) -> &[ScriptedPeer] {
        &self.peers
    }

    /// The registered peer named `name`.
    pub fn peer(&self, name: &str) -> Option<&ScriptedPeer> {
        self.peers.iter().find(|p| p.name == name)
    }

    /// Every scripted peer's dialable multiaddr, for a node's bootnode list.
    pub fn bootnodes(&self) -> Vec<Multiaddr> {
        self.peers.iter().map(|p| p.multiaddr.clone()).collect()
    }

    /// A signed gossip record for the peer named `name`, as hive discovery
    /// would deliver it: the peer's own signature over its name-based
    /// multiaddr, with a strictly increasing timestamp.
    #[allow(clippy::expect_used)]
    pub fn signed_record(&mut self, name: &str) -> SwarmPeer {
        let peer = self
            .peer(name)
            .expect("record for a registered peer")
            .clone();
        let identity = Identity::new(
            host_signer(self.seed, name),
            peer.nonce,
            Arc::clone(&self.spec),
            peer.node_type,
        );
        self.record_clock += 1;
        let timestamp = Timestamp::now().get() + self.record_clock;
        SwarmPeer::sign(
            &identity,
            vec![peer.dns_multiaddr()],
            Timestamp::from_seconds(timestamp),
            None,
        )
        .expect("signing a scripted record cannot fail")
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

/// Grind a nonce so the host's overlay lands exactly in the placement bin
/// (and slot, when pinned).
///
/// The expected cost doubles per bin and multiplies by the slot count when a
/// slot is pinned, so scenarios place peers in shallow bins. Deterministic
/// in (seed, host, spec, placement).
pub fn placement_nonce(world_seed: u64, host: &str, spec: &Spec, placement: Placement) -> Nonce {
    let address = host_signer(world_seed, host).address();
    let mut candidate = placement_digest(world_seed, host);
    loop {
        let nonce = Nonce::new(candidate.0);
        let overlay = compute_overlay(&address, spec.network_id(), &nonce);
        if placement.anchor.proximity(&overlay).get() == placement.bin.get()
            && placement
                .slot
                .is_none_or(|slot| slot_of(&overlay, placement.bin) == slot)
        {
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

/// Advertises one fixed multiaddr in every handshake, so the records peers
/// exchange stay re-dialable (and scope-free) after the exchange.
struct ScriptAddresses(Multiaddr);

impl AddressProvider for ScriptAddresses {
    fn addresses_for_peer(&self, _peer_addr: &Multiaddr) -> Vec<Multiaddr> {
        vec![self.0.clone()]
    }

    fn local_peer_id(&self) -> Option<&PeerId> {
        None
    }
}

async fn run_script(
    ctx: HostContext,
    spec: Arc<Spec>,
    node_type: SwarmNodeType,
    nonce: Nonce,
    script: PeerScript,
    advertised: Multiaddr,
) -> HostResult {
    if matches!(script, PeerScript::Unreachable) {
        futures::future::pending::<()>().await;
        return Ok(());
    }

    // A handshake-failing peer signs its record under an adjacent network
    // id: the transport and muxer negotiate normally, then the dialer's
    // handshake validation rejects the exchange.
    let spec = match script {
        PeerScript::HandshakeFail => Arc::new(
            vertex_swarm_spec::SpecBuilder::testnet()
                .network_id(spec.network_id().get() + 1)
                .bootnodes(Vec::new())
                .build(),
        ),
        _ => spec,
    };

    let identity = Arc::new(Identity::new(ctx.signer(), nonce, spec, node_type));
    let addresses = Arc::new(ScriptAddresses(advertised));
    let swarm = ctx.swarm_with_idle(SimAuth::Plaintext, SCRIPTED_PEER_IDLE, move |_keypair| {
        HandshakeBehaviour::new(identity, addresses, "sim")
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
        PeerScript::DialAndHold { target } => {
            // Let the target's listener come up before the first dial.
            traced.drive_for(Duration::from_millis(200)).await;
            loop {
                if traced.swarm().connected_peers().next().is_none() {
                    let _ = traced.swarm_mut().dial(target.clone());
                    traced.drive_for(Duration::from_secs(1)).await;
                } else {
                    traced.drive_for(Duration::from_secs(5)).await;
                }
            }
        }
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
            let placement = Placement::new(anchor, bin);
            let nonce = placement_nonce(7, "peer", &spec, placement);
            let overlay =
                compute_overlay(&host_signer(7, "peer").address(), spec.network_id(), &nonce);
            assert_eq!(anchor.proximity(&overlay).get(), bin.get());
            // Deterministic: the same inputs grind the same nonce.
            assert_eq!(nonce, placement_nonce(7, "peer", &spec, placement));
        }
    }

    #[test]
    fn placement_lands_the_exact_slot() {
        let spec = test_spec();
        let anchor = OverlayAddress::from([0x55; 32]);
        let bin = Bin::new(1).unwrap_or(Bin::MAX);
        for slot in [0u8, 5, 15] {
            let placement = Placement::new(anchor, bin).in_slot(slot);
            let nonce = placement_nonce(9, "peer", &spec, placement);
            let overlay =
                compute_overlay(&host_signer(9, "peer").address(), spec.network_id(), &nonce);
            assert_eq!(anchor.proximity(&overlay).get(), bin.get());
            assert_eq!(slot_of(&overlay, bin), slot);
        }
    }

    #[test]
    fn churn_selection_is_seeded() {
        fn victims(seed: u64) -> Vec<String> {
            // Fixed: this test compares distinct seeds.
            let mut world = SimWorld::builder()
                .fixed_seed(seed)
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

    #[test]
    fn signed_records_verify_and_stay_fresh() {
        let mut world = SimWorld::builder()
            .seed(11)
            .duration(Duration::from_secs(10))
            .build();
        let mut scenario = Scenario::new(&world, test_spec());
        scenario.add_peer(
            &mut world,
            "peer-0",
            SwarmNodeType::Storer,
            PeerScript::Honest,
            None,
        );

        let first = scenario.signed_record("peer-0");
        let second = scenario.signed_record("peer-0");
        #[allow(clippy::expect_used)]
        let peer = scenario.peer("peer-0").expect("registered");
        assert_eq!(OverlayAddress::from(*first.overlay()), peer.overlay);
        assert_eq!(first.multiaddrs(), &[peer.dns_multiaddr()]);
        assert!(
            second.timestamp().get() > first.timestamp().get(),
            "record timestamps are strictly increasing"
        );
    }
}
