//! Scenario vocabulary over the sim world: scripted peer behaviour, seeded
//! churn and partition schedules, and reusable invariant checks read from
//! live routing statistics.

mod invariant;
mod schedule;
mod script;

pub use invariant::Invariants;
pub use schedule::{Fault, FaultSchedule};
pub use script::{
    PeerScript, Placement, Scenario, ScriptedPeer, handshake_summary, placement_nonce, slot_of,
};
