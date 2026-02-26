//! Generic dial request tracking with bounded queue and in-flight management.

mod config;
mod tracker;
mod types;

pub use config::DialTrackerConfig;
pub use tracker::DialTracker;
pub use types::{CleanupResult, DialCounts, DialDispatch, DialRequest, EnqueueResult};
