//! Fault schedule applied at virtual-time offsets while the world runs.

use std::time::Duration;

use crate::scenario::script::Scenario;
use crate::world::{SimError, SimWorld};

/// A fault the schedule injects into the running world.
#[derive(Clone, Debug)]
pub enum Fault {
    /// Bounce a seeded fraction of the running scripted peers.
    Churn {
        /// Fraction of the running scripted peers to bounce.
        fraction: f64,
    },
    /// Drop all traffic between every host in `a` and every host in `b`.
    Partition {
        /// One side of the cut.
        a: Vec<String>,
        /// The other side of the cut.
        b: Vec<String>,
    },
    /// Restore traffic between every host in `a` and every host in `b`.
    Repair {
        /// One side of the healed cut.
        a: Vec<String>,
        /// The other side of the healed cut.
        b: Vec<String>,
    },
    /// Restart one scripted host, dropping its connections and network state.
    Bounce(String),
    /// Stop one scripted host without restarting it.
    Crash(String),
    /// Change the global message drop probability.
    FailRate(f64),
}

/// Faults keyed by offsets on the world clock ([`SimWorld::elapsed`]).
///
/// Offsets are absolute virtual times, so a schedule built after a
/// convergence phase anchors its offsets on the elapsed time at build.
#[derive(Clone, Debug, Default)]
pub struct FaultSchedule {
    events: Vec<(Duration, Fault)>,
}

impl FaultSchedule {
    /// An empty schedule.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inject `fault` once the world clock reaches `at`.
    pub fn at(mut self, at: Duration, fault: Fault) -> Self {
        self.events.push((at, fault));
        self
    }

    /// Drive the world through every fault in offset order, invoking
    /// `after_each` right after each injection; recovery windows and
    /// invariant assertions belong in that callback.
    ///
    /// The world must hold at least one incomplete client: once every
    /// client completes, stepping stops advancing virtual time.
    pub fn apply(
        mut self,
        world: &mut SimWorld,
        scenario: &mut Scenario,
        mut after_each: impl FnMut(&mut SimWorld, &Fault),
    ) -> Result<(), SimError> {
        // Stable sort: same-offset faults inject in insertion order.
        self.events.sort_by_key(|(at, _)| *at);
        for (at, fault) in &self.events {
            let now = world.elapsed();
            if *at > now {
                world.run_for(*at - now)?;
            }
            inject(world, scenario, fault);
            after_each(world, fault);
        }
        Ok(())
    }
}

fn inject(world: &mut SimWorld, scenario: &mut Scenario, fault: &Fault) {
    match fault {
        Fault::Churn { fraction } => {
            scenario.churn_burst(world, *fraction);
        }
        Fault::Partition { a, b } => {
            for left in a {
                for right in b {
                    world.partition(left, right);
                }
            }
        }
        Fault::Repair { a, b } => {
            for left in a {
                for right in b {
                    world.repair(left, right);
                }
            }
        }
        Fault::Bounce(name) => world.bounce(name),
        Fault::Crash(name) => world.crash(name),
        Fault::FailRate(rate) => world.set_fail_rate(*rate),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use crate::HostContext;
    use crate::world::HostResult;
    use vertex_swarm_test_utils::test_spec;

    async fn idle(_ctx: HostContext) -> HostResult {
        futures::future::pending::<()>().await;
        Ok(())
    }

    #[test]
    fn faults_apply_in_offset_order() {
        let mut world = SimWorld::builder()
            .seed(3)
            .duration(Duration::from_secs(60))
            .build();
        world.host("a", idle);
        world.host("b", idle);
        // A pending client keeps the world stepping: with every client
        // complete, `run_for` would finish without advancing virtual time.
        world.client("driver", |_ctx| async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(())
        });
        let mut scenario = Scenario::new(&world, test_spec());

        let mut seen: Vec<(Duration, bool)> = Vec::new();
        FaultSchedule::new()
            .at(
                Duration::from_secs(10),
                Fault::Repair {
                    a: vec!["a".into()],
                    b: vec!["b".into()],
                },
            )
            .at(
                Duration::from_secs(5),
                Fault::Partition {
                    a: vec!["a".into()],
                    b: vec!["b".into()],
                },
            )
            .apply(&mut world, &mut scenario, |world, fault| {
                seen.push((world.elapsed(), matches!(fault, Fault::Partition { .. })));
            })
            .expect("schedule applies");

        assert_eq!(seen.len(), 2);
        assert!(seen[0].1, "the earlier partition injects first");
        assert!(seen[0].0 >= Duration::from_secs(5));
        assert!(!seen[1].1, "the later repair injects second");
        assert!(seen[1].0 >= Duration::from_secs(10));
    }
}
