//! Generic dial request tracking with bounded queue and in-flight management.

mod config;
mod prepare;
mod tracker;
mod types;

pub use config::DialTrackerConfig;
pub use prepare::{PrepareError, prepare_dial_opts};
pub use tracker::DialTracker;
pub use types::{CleanupResult, DialDispatch, DialRequest, EnqueueResult};
