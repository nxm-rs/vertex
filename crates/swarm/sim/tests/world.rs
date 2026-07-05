//! World-level coverage: scale, fault injection, and restart identity.
#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use libp2p::swarm::SwarmEvent;
use vertex_swarm_identity::Identity;
use vertex_swarm_net_handshake::{HandshakeBehaviour, HandshakeEvent, NoAddresses};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_sim::{HostContext, HostResult, SimAuth, SimWorld, TracedSwarm, listen_multiaddr};
use vertex_swarm_test_utils::test_spec;

const PORT: u16 = 1634;

type Behaviour = HandshakeBehaviour<Identity, NoAddresses>;

fn handshake_summary(event: &HandshakeEvent) -> String {
    match event {
        HandshakeEvent::Completed { peer_id, .. } => format!("handshake-completed peer={peer_id}"),
        HandshakeEvent::Failed { peer_id, error, .. } => {
            format!("handshake-failed peer={peer_id} error={error}")
        }
    }
}

fn build_traced(ctx: &HostContext) -> TracedSwarm<Behaviour> {
    let identity = Arc::new(ctx.identity(test_spec(), SwarmNodeType::Client));
    let swarm = ctx.swarm(SimAuth::Plaintext, move |_keypair| {
        HandshakeBehaviour::new(identity, Arc::new(NoAddresses), "sim")
    });
    ctx.traced(swarm, handshake_summary)
}

fn is_completed(event: &SwarmEvent<HandshakeEvent>) -> bool {
    matches!(
        event,
        SwarmEvent::Behaviour(HandshakeEvent::Completed { .. })
    )
}

/// An eternal listener host serving handshakes for the whole run.
async fn serve(ctx: HostContext) -> HostResult {
    let mut traced = build_traced(&ctx);
    traced.swarm_mut().listen_on(listen_multiaddr(PORT))?;
    traced.drive_for(Duration::from_secs(3600)).await;
    Ok(())
}

/// The scale contract: a 100-host world constructs and a handful of
/// handshakes across it complete in seconds of wall time under virtual time.
#[test]
fn hundred_host_world_completes_star_handshakes() {
    const LISTENERS: usize = 96;
    const DIALERS: usize = 4;

    let mut world = SimWorld::builder()
        .seed(1)
        .duration(Duration::from_secs(120))
        .build();

    for i in 0..LISTENERS {
        world.host(&format!("listener-{i}"), serve);
    }
    for i in 0..DIALERS {
        let target = format!("listener-{i}");
        world.client(&format!("dialer-{i}"), move |ctx| async move {
            let mut traced = build_traced(&ctx);
            tokio::time::sleep(Duration::from_millis(200)).await;
            traced
                .swarm_mut()
                .dial(ctx.lookup_multiaddr(&target, PORT))?;
            if !traced
                .drive_until(Duration::from_secs(60), is_completed)
                .await
            {
                return Err("handshake did not complete".into());
            }
            Ok(())
        });
    }

    world.run();

    let lines = world.trace_lines();
    for i in 0..DIALERS {
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("dialer-{i}")) && l.contains("handshake-completed")),
            "dialer-{i} never completed (seed={}): {lines:#?}",
            world.seed()
        );
    }
}

/// A partition blocks the handshake; repairing the link heals it.
#[test]
fn partition_blocks_and_repair_heals() {
    let mut world = SimWorld::builder()
        .seed(5)
        .duration(Duration::from_secs(120))
        .build();

    world.host("server", serve);
    world.client("dialer", |ctx| async move {
        let addr = ctx.lookup_multiaddr("server", PORT);
        let mut traced = build_traced(&ctx);
        traced.swarm_mut().dial(addr.clone())?;
        if traced
            .drive_until(Duration::from_secs(3), is_completed)
            .await
        {
            return Err("handshake completed across a partition".into());
        }
        ctx.record("first attempt blocked");
        // Redial well after the driver repairs the link at t=5s.
        tokio::time::sleep(Duration::from_secs(7)).await;
        traced.swarm_mut().dial(addr)?;
        if !traced
            .drive_until(Duration::from_secs(30), is_completed)
            .await
        {
            return Err("handshake did not complete after repair".into());
        }
        Ok(())
    });

    world.partition("dialer", "server");
    world
        .run_for(Duration::from_secs(5))
        .expect("pre-repair run");
    world.repair("dialer", "server");
    world.run();

    let lines = world.trace_lines();
    assert!(
        lines.iter().any(|l| l.contains("first attempt blocked")),
        "partition did not block (seed={}): {lines:#?}",
        world.seed()
    );
}

/// Virtual completion time of one dial-and-handshake under a latency range.
fn handshake_completion_time(min: Duration, max: Duration) -> Duration {
    let mut world = SimWorld::builder()
        .fixed_seed(11)
        .duration(Duration::from_secs(120))
        .latency(min, max)
        .build();
    world.host("server", serve);
    world.client("dialer", |ctx| async move {
        let mut traced = build_traced(&ctx);
        traced
            .swarm_mut()
            .dial(ctx.lookup_multiaddr("server", PORT))?;
        if !traced
            .drive_until(Duration::from_secs(60), is_completed)
            .await
        {
            return Err("handshake did not complete".into());
        }
        Ok(())
    });
    world.run();
    world.elapsed()
}

/// The latency control reaches the schedule: the same seeded world finishes
/// its handshake later under a slower link.
#[test]
fn latency_shifts_the_virtual_completion_time() {
    let fast = handshake_completion_time(Duration::from_millis(1), Duration::from_millis(2));
    let slow = handshake_completion_time(Duration::from_millis(400), Duration::from_millis(500));
    assert!(
        slow > fast + Duration::from_millis(500),
        "latency range had no effect: fast={fast:?} slow={slow:?}"
    );
}

/// The fail_rate control reaches the schedule: dropping every message blocks
/// the handshake entirely.
#[test]
fn full_fail_rate_blocks_the_handshake() {
    let mut world = SimWorld::builder()
        .fixed_seed(7)
        .duration(Duration::from_secs(60))
        .fail_rate(1.0)
        .build();
    world.host("server", serve);
    world.client("dialer", |ctx| async move {
        let mut traced = build_traced(&ctx);
        traced
            .swarm_mut()
            .dial(ctx.lookup_multiaddr("server", PORT))?;
        if traced
            .drive_until(Duration::from_secs(30), is_completed)
            .await
        {
            return Err("handshake completed with every message dropped".into());
        }
        Ok(())
    });
    world.run();
}

/// A bounced host restarts with the same derived identity.
#[test]
fn bounce_preserves_host_identity() {
    let mut world = SimWorld::builder()
        .seed(3)
        .duration(Duration::from_secs(120))
        .build();

    world.host("svc", |ctx| async move {
        ctx.record(format!("incarnation peer_id={}", ctx.peer_id()));
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Ok(())
    });
    world.client("observer", |_ctx| async {
        tokio::time::sleep(Duration::from_secs(15)).await;
        Ok(())
    });

    world
        .run_for(Duration::from_secs(5))
        .expect("pre-bounce run");
    world.bounce("svc");
    world.run();

    let incarnations: Vec<_> = world
        .trace_entries()
        .into_iter()
        .filter(|e| e.host == "svc" && e.line.starts_with("incarnation"))
        .collect();
    assert_eq!(
        incarnations.len(),
        2,
        "expected one record per incarnation (seed={})",
        world.seed()
    );
    let unique: std::collections::HashSet<_> =
        incarnations.iter().map(|e| e.line.as_str()).collect();
    assert_eq!(
        unique.len(),
        1,
        "identity changed across restart (seed={}): {incarnations:#?}",
        world.seed()
    );
}
