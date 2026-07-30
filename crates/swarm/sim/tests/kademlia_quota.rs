//! Minimum-outbound quota: a bin whose count target is met by an inbound
//! flood still pulls self-dialed connections up to the quota, holds without
//! a dial storm, and drains an inbound leaver exactly.

mod common;

use std::time::Duration;

use common::{
    NODE, NODE_PORT, await_handle, connected_in_bin, family, gossip, launch_node, node_overlay,
    run_until, spec, trace_count,
};
use vertex_swarm_primitives::{Bin, SwarmNodeType};
use vertex_swarm_sim::{PeerScript, Placement, Scenario, SimWorld};
use vertex_swarm_topology::KademliaConfig;

const SEED: u64 = 13;
const STORER: SwarmNodeType = SwarmNodeType::Storer;
/// Half the default saturation threshold, rounded up.
const QUOTA: usize = 4;

#[test]
fn outbound_quota_holds_under_inbound_flood() {
    let mut world = SimWorld::builder()
        .seed(SEED)
        .duration(Duration::from_secs(48 * 3600))
        .tick(Duration::from_millis(50))
        .tokio_io()
        .build();
    // total_target 8 pins bin 0's dial target at exactly saturation, so the
    // flood alone satisfies the count and only the outbound quota can
    // justify further dials.
    let probe = launch_node(&mut world, KademliaConfig::default().with_total_target(8));
    // The node's libp2p keypair is freshly generated inside the build, so
    // the flood dials the bare address and learns the peer id on the wire.
    let node_addr = world.multiaddr_of(NODE, NODE_PORT);

    // Every bin-0 peer shares one sub-prefix slot, so the empty-slot pass
    // stays quiet and the quota is the only path into the flooded bin.
    let anchor = node_overlay(SEED);
    let slot0 = Some(Placement::new(anchor, Bin::new(0).unwrap_or(Bin::MAX)).in_slot(0));
    let mut scenario = Scenario::new(&world, spec());
    let mut anchors = Vec::new();
    for name in family("anchor", 3) {
        scenario.add_peer(
            &mut world,
            &name,
            STORER,
            PeerScript::Honest,
            Some(Placement::new(anchor, Bin::new(1).unwrap_or(Bin::MAX))),
        );
        anchors.push(name);
    }
    // The flood: eight attackers dial the node and hold, never gossiped.
    let attackers = family("attacker", 8);
    for name in &attackers {
        scenario.add_peer(
            &mut world,
            name,
            STORER,
            PeerScript::DialAndHold {
                target: node_addr.clone(),
            },
            slot0,
        );
    }
    // The outbound supply: six known dialable peers, of which only the quota
    // may be dialed once the flood has met the count target.
    let supply = family("supply", 6);
    for name in &supply {
        scenario.add_peer(&mut world, name, STORER, PeerScript::Honest, slot0);
    }

    let handle = await_handle(&mut world, &probe);
    gossip(&handle, &mut scenario, &anchors);

    // The attacker flood meets bin 0's count target by inbound alone.
    let flooded = run_until(
        &mut world,
        Duration::from_secs(600),
        Duration::from_secs(2),
        || {
            let stats = handle.routing_stats();
            stats.depth == 1 && connected_in_bin(&stats, 0) == 8
        },
    );
    assert!(
        flooded,
        "the inbound flood never met the count target (seed={SEED}): {:?}",
        handle.routing_stats()
    );
    let supply_dials = |world: &SimWorld| -> usize {
        supply
            .iter()
            .map(|name| trace_count(world, name, "incoming"))
            .filter(|count| *count > 0)
            .count()
    };
    assert_eq!(supply_dials(&world), 0, "no outbound supply is known yet");

    // The direction-aware fill pulls outbound dials into the flooded bin
    // even though the count target is already met, and stops at the quota.
    gossip(&handle, &mut scenario, &supply);
    let quota_met = run_until(
        &mut world,
        Duration::from_secs(300),
        Duration::from_secs(2),
        || connected_in_bin(&handle.routing_stats(), 0) == 8 + QUOTA,
    );
    assert!(
        quota_met,
        "the quota never pulled outbound dials into the flooded bin (seed={SEED}): {:?}",
        handle.routing_stats()
    );
    assert_eq!(
        supply_dials(&world),
        QUOTA,
        "exactly the quota of supply peers is self-dialed (seed={SEED})"
    );

    // No storm: with the quota met the evaluator falls quiet; the bin holds
    // steady rather than dialling the rest of the supply.
    world
        .run_for(Duration::from_secs(300))
        .expect("world advances");
    assert_eq!(
        connected_in_bin(&handle.routing_stats(), 0),
        8 + QUOTA,
        "the quota-filled bin holds without a dial storm (seed={SEED})"
    );
    assert_eq!(
        supply_dials(&world),
        QUOTA,
        "the outbound share stays at the quota"
    );

    // One flooded inbound peer that reached Active now leaves. The drain is
    // exact: the connected count drops by one, the outbound share holds, and
    // with the count target still met the evaluator stays quiet.
    world.crash("attacker-0");
    let drained = run_until(
        &mut world,
        Duration::from_secs(60),
        Duration::from_millis(250),
        || connected_in_bin(&handle.routing_stats(), 0) == 7 + QUOTA,
    );
    assert!(
        drained,
        "the inbound leaver never drained (seed={SEED}): {:?}",
        handle.routing_stats()
    );
    world
        .run_for(Duration::from_secs(120))
        .expect("world advances");
    assert_eq!(
        connected_in_bin(&handle.routing_stats(), 0),
        7 + QUOTA,
        "the drained bin holds steady: the count target is still met (seed={SEED})"
    );
    assert_eq!(
        supply_dials(&world),
        QUOTA,
        "an inbound leave never moves the outbound share (seed={SEED})"
    );
}
