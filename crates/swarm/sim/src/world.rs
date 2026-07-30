//! Seeded simulation world over turmoil.
//!
//! One world per test, run on the current thread. Hosts are named and
//! registered in deterministic order: turmoil assigns IPs by registration
//! order and counts ephemeral ports per host, so registration order is part
//! of the seed surface.

use std::future::Future;
use std::net::IpAddr;
use std::time::Duration;

use libp2p::{Multiaddr, multiaddr::Protocol};

use crate::host::HostContext;
use crate::trace::{SimTrace, TraceEntry};

/// Outcome a host future reports to the world.
pub type HostResult = turmoil::Result;

/// Environment variable forcing a replay seed onto every world built with
/// [`SimWorldBuilder::seed`].
pub const SEED_ENV: &str = "VERTEX_SIM_SEED";

#[allow(clippy::panic)]
fn env_seed() -> Option<u64> {
    let value = std::env::var(SEED_ENV).ok()?;
    match value.parse() {
        Ok(seed) => Some(seed),
        Err(_) => panic!("{SEED_ENV} must be a u64, got {value:?}"),
    }
}

/// A failed run, carrying the seed that replays it exactly.
#[derive(Debug, thiserror::Error)]
#[error("sim failed (seed={seed}): {message}")]
pub struct SimError {
    /// Seed of the failed world.
    pub seed: u64,
    /// Rendered failure from the simulation.
    pub message: String,
}

/// Listen multiaddr for the unspecified address on `port`.
///
/// turmoil listeners bind only unspecified or loopback addresses; hosts are
/// then reached through their assigned IPs.
#[allow(clippy::expect_used)]
pub fn listen_multiaddr(port: u16) -> Multiaddr {
    format!("/ip6/::/tcp/{port}")
        .parse()
        .expect("literal listen multiaddr is well-formed")
}

/// Builder mapping seed, latency, tick, and duration controls onto the
/// underlying simulation.
#[derive(Debug, Clone, Default)]
pub struct SimWorldBuilder {
    seed: u64,
    seed_pinned: bool,
    duration: Option<Duration>,
    tick: Option<Duration>,
    latency: Option<(Duration, Duration)>,
    fail_rate: Option<f64>,
    tokio_io: bool,
}

impl SimWorldBuilder {
    /// Seed for the whole world: message latencies, drop draws, and every
    /// host identity derive from it. Defaults to 0.
    ///
    /// [`SEED_ENV`] overrides it at build, so a failed run replays exactly
    /// from its reported seed. Derive host material from
    /// [`SimWorld::seed`], never from the value passed here.
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Seed exempt from the [`SEED_ENV`] override, for determinism proofs
    /// that compare worlds built from distinct seeds.
    pub fn fixed_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self.seed_pinned = true;
        self
    }

    /// Maximum virtual duration before the run fails.
    pub fn duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    /// Virtual time advanced per scheduler tick.
    pub fn tick(mut self, tick: Duration) -> Self {
        self.tick = Some(tick);
        self
    }

    /// Message latency range links draw from.
    pub fn latency(mut self, min: Duration, max: Duration) -> Self {
        self.latency = Some((min, max));
        self
    }

    /// Probability that a message is dropped.
    pub fn fail_rate(mut self, rate: f64) -> Self {
        self.fail_rate = Some(rate);
        self
    }

    /// Enable the real tokio IO driver inside host runtimes.
    ///
    /// Whole-node hosts need it (the topology interface watcher opens OS
    /// sockets); leave it off for pure swarm hosts so no real IO can leak
    /// into the schedule.
    pub fn tokio_io(mut self) -> Self {
        self.tokio_io = true;
        self
    }

    /// Build the world.
    ///
    /// The network is IPv6 link-local: those are the addresses the production
    /// dial filter admits without consulting the real host's routing state,
    /// so whole-node dialling stays deterministic across machines.
    pub fn build(self) -> SimWorld {
        let seed = match (self.seed_pinned, env_seed()) {
            (false, Some(seed)) => seed,
            _ => self.seed,
        };
        let mut builder = turmoil::Builder::new();
        builder.ip_version(turmoil::IpVersion::V6);
        builder.rng_seed(seed);
        if let Some(duration) = self.duration {
            builder.simulation_duration(duration);
        }
        if let Some(tick) = self.tick {
            builder.tick_duration(tick);
        }
        if let Some((min, max)) = self.latency {
            builder.min_message_latency(min);
            builder.max_message_latency(max);
        }
        if let Some(rate) = self.fail_rate {
            builder.fail_rate(rate);
        }
        if self.tokio_io {
            builder.enable_tokio_io();
        }
        SimWorld {
            sim: builder.build(),
            seed,
            trace: SimTrace::default(),
        }
    }
}

/// A seeded, virtual-time world hosting vertex swarms and nodes.
pub struct SimWorld {
    sim: turmoil::Sim<'static>,
    seed: u64,
    trace: SimTrace,
}

impl SimWorld {
    /// Start a world blueprint.
    pub fn builder() -> SimWorldBuilder {
        SimWorldBuilder::default()
    }

    /// The world seed.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The world trace.
    pub fn trace(&self) -> &SimTrace {
        &self.trace
    }

    /// The world trace as formatted lines, for seed-replay comparisons.
    pub fn trace_lines(&self) -> Vec<String> {
        self.trace.lines()
    }

    /// The world trace entries.
    pub fn trace_entries(&self) -> Vec<TraceEntry> {
        self.trace.entries()
    }

    fn context(&self, name: &str) -> HostContext {
        HostContext::new(self.seed, name, self.trace.clone())
    }

    /// Register a host whose future must complete for the run to succeed.
    pub fn client<F>(&mut self, name: &str, f: impl FnOnce(HostContext) -> F)
    where
        F: Future<Output = HostResult> + 'static,
    {
        let ctx = self.context(name);
        self.sim.client(name, f(ctx));
    }

    /// Register a restartable host: the closure is re-invoked after a
    /// [`bounce`](Self::bounce), with a fresh context deriving the same
    /// identity, and the future is expected to run for the whole simulation.
    pub fn host<F, Fut>(&mut self, name: &str, f: F)
    where
        F: Fn(HostContext) -> Fut + 'static,
        Fut: Future<Output = HostResult> + 'static,
    {
        let ctx = self.context(name);
        self.sim.host(name, move || f(ctx.clone()));
    }

    /// Resolve a registered host name to its IP.
    pub fn lookup(&self, name: &str) -> IpAddr {
        self.sim.lookup(name)
    }

    /// Dialable multiaddr for a registered host on `port`.
    pub fn multiaddr_of(&self, name: &str, port: u16) -> Multiaddr {
        Multiaddr::empty()
            .with(self.lookup(name).into())
            .with(Protocol::Tcp(port))
    }

    /// Drop all traffic between two hosts.
    ///
    /// A dial across the partition errors immediately in virtual time; to
    /// model blackholed traffic that hangs until a timeout, use
    /// [`hold`](Self::hold) instead.
    pub fn partition(&self, a: &str, b: &str) {
        self.sim.partition(a, b);
    }

    /// Drop traffic flowing from `from` to `to` only.
    pub fn partition_oneway(&self, from: &str, to: &str) {
        self.sim.partition_oneway(from, to);
    }

    /// Restore traffic between two hosts.
    pub fn repair(&self, a: &str, b: &str) {
        self.sim.repair(a, b);
    }

    /// Restore traffic flowing from `from` to `to` only.
    pub fn repair_oneway(&self, from: &str, to: &str) {
        self.sim.repair_oneway(from, to);
    }

    /// Queue traffic between two hosts instead of delivering it.
    pub fn hold(&self, a: &str, b: &str) {
        self.sim.hold(a, b);
    }

    /// Deliver traffic previously held between two hosts.
    pub fn release(&self, a: &str, b: &str) {
        self.sim.release(a, b);
    }

    /// Restart a restartable host: its software stops, its network state
    /// drops, and the registered closure runs again.
    pub fn bounce(&mut self, name: &str) {
        self.sim.bounce(name);
    }

    /// Crash a restartable host without restarting it.
    pub fn crash(&mut self, name: &str) {
        self.sim.crash(name);
    }

    /// Whether a restartable host is currently running.
    pub fn is_host_running(&mut self, name: &str) -> bool {
        self.sim.is_host_running(name)
    }

    /// Change the global message drop probability mid-run.
    pub fn set_fail_rate(&mut self, rate: f64) {
        self.sim.set_fail_rate(rate);
    }

    /// Change the drop probability on one link mid-run.
    pub fn set_link_fail_rate(&mut self, a: &str, b: &str, rate: f64) {
        self.sim.set_link_fail_rate(a, b, rate);
    }

    /// Change the global maximum message latency mid-run.
    pub fn set_max_message_latency(&self, value: Duration) {
        self.sim.set_max_message_latency(value);
    }

    /// Pin the latency of one link mid-run.
    pub fn set_link_latency(&self, a: &str, b: &str, value: Duration) {
        self.sim.set_link_latency(a, b, value);
    }

    /// Virtual time elapsed so far.
    pub fn elapsed(&self) -> Duration {
        self.sim.elapsed()
    }

    /// Run until every client completes, panicking with the seed on failure.
    #[allow(clippy::panic)]
    pub fn run(&mut self) {
        if let Err(error) = self.try_run() {
            panic!("{error}");
        }
    }

    /// Run until every client completes.
    pub fn try_run(&mut self) -> Result<(), SimError> {
        self.sim.run().map_err(|e| self.error(&*e))
    }

    /// Advance the world one tick; `Ok(true)` once every client completed.
    pub fn step(&mut self) -> Result<bool, SimError> {
        self.sim.step().map_err(|e| self.error(&*e))
    }

    /// Advance the world by `duration` of virtual time (or until every client
    /// completes), so fault injection can interleave with execution.
    pub fn run_for(&mut self, duration: Duration) -> Result<(), SimError> {
        let target = self.sim.elapsed() + duration;
        while self.sim.elapsed() < target {
            if self.step()? {
                break;
            }
        }
        Ok(())
    }

    fn error(&self, error: &(dyn std::error::Error + 'static)) -> SimError {
        SimError {
            seed: self.seed,
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_multiaddr_is_wellformed() {
        assert_eq!(listen_multiaddr(1634).to_string(), "/ip6/::/tcp/1634");
    }

    #[test]
    fn env_overrides_the_default_seed() {
        // SAFETY: under nextest every test owns its process; under plain
        // cargo test the concurrently racing tests are seed-independent.
        unsafe { std::env::set_var(SEED_ENV, "1234") };
        let replayed = SimWorld::builder().seed(1).build();
        let pinned = SimWorld::builder().fixed_seed(1).build();
        // SAFETY: as above.
        unsafe { std::env::remove_var(SEED_ENV) };
        assert_eq!(replayed.seed(), 1234, "the env seed forces the replay");
        assert_eq!(pinned.seed(), 1, "a fixed seed ignores the override");
    }

    #[test]
    fn failure_reports_seed() {
        let mut world = SimWorld::builder().fixed_seed(99).build();
        world.client("boom", |_ctx| async { Err("boom".into()) });
        let error = world.try_run().expect_err("client fails the run");
        let rendered = error.to_string();
        assert!(rendered.contains("seed=99"), "missing seed: {rendered}");
        assert!(rendered.contains("boom"), "missing cause: {rendered}");
    }

    #[test]
    fn hosts_get_deterministic_addresses() {
        let mut world = SimWorld::builder().seed(1).build();
        world.client("first", |_ctx| async { Ok(()) });
        world.client("second", |_ctx| async { Ok(()) });
        // Registration order fixes IP assignment, part of the seed surface.
        assert_eq!(
            world.multiaddr_of("first", 7).to_string(),
            "/ip6/fe80::1/tcp/7"
        );
        assert_eq!(
            world.multiaddr_of("second", 7).to_string(),
            "/ip6/fe80::2/tcp/7"
        );
        world.run();
    }
}
